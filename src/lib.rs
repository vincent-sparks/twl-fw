#![feature(get_mut_unchecked)]
use tokio::sync::{oneshot, mpsc, Mutex};
use twilight_model::application::interaction::{Interaction, InteractionType, InteractionData, application_command::CommandData, modal::ModalInteractionData, message_component::MessageComponentInteractionData};
use tracing::{info};
use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};
use twilight_http::Client;
use twilight_model::http::interaction::{InteractionResponse, InteractionResponseType, InteractionResponseData};
use twilight_model::channel::message::component::{ActionRow,TextInput,Component};
use twilight_model::id::{Id, marker::InteractionMarker};
use twilight_util::builder::InteractionResponseDataBuilder;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use rand::{thread_rng, seq::SliceRandom};
use once_cell::sync::Lazy;

pub type CommandFunc = Box<dyn Fn(Arc<InteractionHandler>, Interaction, CommandData) -> Pin<Box<dyn Future<Output=anyhow::Result<()>> + Send>> + Send + Sync>;

pub type CommandMap = phf::Map<&'static str, &'static Lazy<CommandFunc>>;

#[derive(Default)]
struct _CommandIDMappings {
    interaction_id_to_command_id: HashMap<Id<InteractionMarker>, usize>,
    command_id_to_interaction: HashMap<usize, (Id<InteractionMarker>, String)>,
}

#[cfg(not(test))]
type PossiblyFakeClient = Client;
#[cfg(test)]
type PossiblyFakeClient = tests::FakeClient;

pub struct InteractionHandler {
    pub client: Arc<PossiblyFakeClient>,
    commands: &'static CommandMap,
    modal_waiters: Mutex<HashMap<String, oneshot::Sender<ModalInteractionData>>>,
    message_waiters: Mutex<HashMap<String, oneshot::Sender<MessageComponentInteractionData>>>,
    message_waiters_multishot: Mutex<HashMap<String, mpsc::Sender<MessageComponentInteractionData>>>,
    
    current_command_id: AtomicUsize,
    command_id_mappings: Mutex<_CommandIDMappings>,
}

// I would have liked to build this from a string, but I don't know if you can do that at compile time.
const CUSTOM_ID_CHARS: &'static [char] = &['a','b','c','d','e','f','g','h','i','j','k','l','m','n','o','p','q','r','s','t','u','v','w','x','y','z','A','B','C','D','E','F','G','H','I','J','K','L','M','N','O','P','Q','R','S','T','U','V','W','X','Y','Z','0','1','2','3','4','5','6','7','8','9',];
const CUSTOM_ID_LEN: usize = 6;


fn build_custom_id(ctx: &str) -> String {
    let mut custom_id = String::from(ctx);
    custom_id.push('-');
    custom_id.extend(CUSTOM_ID_CHARS.choose_multiple(&mut thread_rng(), CUSTOM_ID_LEN));
    custom_id
}

macro_rules! log_err {
    ($expr:expr, $message: literal $(, $arg: expr)*) => {
        if let Err(e) = $expr {
            tracing::error!(?e, $message $(, $arg)*);
        }
    }
}

macro_rules! unwrap_or_log {
    ($expr: expr, $message: literal $(, $arg: expr)*) => {
        match $expr {
            Some(x) => x,
            None => {
                tracing::error!($message $(, $arg)*);
                return;
            }
        }
    }
}

#[macro_export]
macro_rules! build_command {
    (|$client: ident, $inter: ident, $data: ident| $($body:tt)*) => {
        ::once_cell::sync::Lazy::new(|| ::std::boxed::Box::new(|$client, $inter, $data| ::std::boxed::Box::pin({$($body)*})))
    }
}

impl InteractionHandler {
    pub fn new(client: Arc<PossiblyFakeClient>, commands: &'static CommandMap) -> Self {
        Self {
            client, commands,
            current_command_id: AtomicUsize::new(0),
            message_waiters: Default::default(),
            message_waiters_multishot: Default::default(),
            modal_waiters: Default::default(),
            command_id_mappings: Default::default(),
        }
    }

    pub async fn handle(self: Arc<Self>, mut interaction: Interaction) {
        if interaction.kind == InteractionType::Ping {
            info!("Got a ping from the Discord API");
            let interaction_client = self.client.interaction(interaction.application_id);
            log_err!(interaction_client.create_response(interaction.id, &interaction.token, &InteractionResponse {
                kind: InteractionResponseType::Pong, data: None
            }).await, "Error answering ping");
            return;
        }
        // ping is the only interaction type that doesn't have data, so the unwrap here should be
        // OK
        match interaction.data.take().unwrap() {
            InteractionData::ApplicationCommand(data) => {
                if let Some(func) = self.commands.get(&data.name) {
                    let id = self.current_command_id.fetch_add(1, Ordering::Relaxed);
                    // the app ID shouldn't be able to change between interactions in the same
                    // chain but this still makes me nervous.
                    let app_id = interaction.application_id;
                    let mut mappings = self.command_id_mappings.lock().await;
                    mappings.interaction_id_to_command_id.insert(interaction.id, id);
                    mappings.command_id_to_interaction.insert(id, (interaction.id, interaction.token.clone()));
                    std::mem::drop(mappings);

                    let this = self.clone();
                    let fut = func(self.clone(), interaction, *data);
                    let fut = async move {
                        let res = fut.await;

                        let mut mappings = this.command_id_mappings.lock().await;
                        let inter = mappings.command_id_to_interaction.remove(&id);

                        if let Some((id, _)) = &inter {
                            // tiny memory leaks are still memory leaks!
                            mappings.interaction_id_to_command_id.remove(id);
                        }

                        if let Err(e) = res {
                            if let Some((id, token)) = inter {
                                log_err!(this.client.interaction(app_id).create_response(id, &token, &InteractionResponse {
                                    kind: InteractionResponseType::ChannelMessageWithSource,
                                    data: Some(InteractionResponseDataBuilder::new()
                                               .content(format!("Internal error! {}", e))
                                               .build()),
                                }).await, "error notifying user of error: {}", e);
                            } else {
                                tracing::error!(?e, "an error occurred in a slash command but no interaction was present to report it");
                            }
                        }
                    };
                    #[cfg(test)]
                    fut.await;
                    #[cfg(not(test))]
                    tokio::spawn(fut);
                }
            },
            InteractionData::ModalSubmit(modal) => {
                let waiter = unwrap_or_log!(self.modal_waiters.lock().await.remove(&modal.custom_id), "unknown custom id: {}", &modal.custom_id);
                unwrap_or_log!(waiter.send(modal).ok(), "task was cancelled while waiting for an interaction");
            },
            InteractionData::MessageComponent(message) => {
                let mut waiters = self.message_waiters_multishot.lock().await;
                let id = message.custom_id.clone();
                let waiter = unwrap_or_log!(waiters.get_mut(&message.custom_id), "unknown custom id: {}", &message.custom_id);
                if waiter.send(message).await.is_err() {
                    // the task dropped the receiver, which means they no longer care.
                    waiters.remove(&id);
                    // ... and clear the interaction buttons from the message.
                    //log_err!(self.client.interaction(interaction.application_id).update_response(&interaction.token).components(None).unwrap().await, "removing components from message");
                }
            },
            _ => todo!(),
        }
    }
    pub async fn show_modal(&self, interaction: &Interaction, fields: Vec<TextInput>, id: &'static str) -> Result<ModalInteractionData, twilight_http::Error> {
        let components: Vec<Component> = fields.into_iter().map(Component::TextInput).collect();
        let (sender, receiver) = oneshot::channel();
        let custom_id = build_custom_id(id);
        self.modal_waiters.lock().await.insert(custom_id.clone(), sender);
        self.client.interaction(interaction.application_id).create_response(interaction.id, &interaction.token, &InteractionResponse {
            kind: InteractionResponseType::Modal,
            data: Some(InteractionResponseDataBuilder::new()
                       .components(components)
                       .custom_id(custom_id)
                       .build()),
        }).await?;

        // receiver wil only fail if the sender is dropped before it sends
        // that can theoretically only happen if self gets dropped while a future that depends on
        // it is still running
        Ok(receiver.await.unwrap())
    }
    pub async fn send_response_with_components(&self, interaction: &Interaction, mut response: InteractionResponseData) -> Result<MessageComponentInteractionData, twilight_http::Error> {
        if response.custom_id.is_none() {
            response.custom_id = Some(build_custom_id(""));
        }
        let (sender, receiver) = oneshot::channel();
        self.message_waiters.lock().await.insert(response.custom_id.clone().unwrap(), sender);
        self.client.interaction(interaction.application_id).create_response(interaction.id, &interaction.token, &InteractionResponse {kind: InteractionResponseType::UpdateMessage, data: Some(response)}).await?;
        Ok(receiver.await.unwrap())
    }

    pub async fn manually_add_component_interaction_waiter(&self, custom_id: String) -> oneshot::Receiver<MessageComponentInteractionData> {
        let (sender, receiver) = oneshot::channel();
        self.message_waiters.lock().await.insert(custom_id, sender);
        receiver
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use phf::phf_map;
    use std::time::Duration;
    static ERROR_COMMAND: Lazy<CommandFunc> = build_command!(|_client, _inter, __data| async {anyhow::bail!("yeet");});
    static HELLO_COMMAND: Lazy<CommandFunc> = build_command!(|h, inter, _data| async move {
        h.client.interaction(inter.application_id).create_response(inter.id, &inter.token, 
                                                                   &InteractionResponse {
                                                                       kind: InteractionResponseType::ChannelMessageWithSource,
                                                                       data: Some(InteractionResponseDataBuilder::new().content("yo").build())
                                                                   }).await?;
        Ok::<_,anyhow::Error>(())
    });

    static MODAL_COMMAND: Lazy<CommandFunc> = build_command!(|h, inter, _data| async move {
        let r = h.show_modal(&inter, vec![], "showin the modal").await.unwrap();
        dbg!(r);
        anyhow::bail!("yeet");
    });


    use twilight_model::id::marker::ApplicationMarker;
    use twilight_model::application::command::CommandType;

    static COMMANDS: CommandMap = phf_map!{
        "hello" => &HELLO_COMMAND,
        "error" => &ERROR_COMMAND,
        "modal" => &MODAL_COMMAND,
    };

    #[derive(Default)]
    pub struct FakeClient {
        interaction_responses: Mutex<Vec<(Id<InteractionMarker>, InteractionResponse)>>,
        reply_fn: Option<Box<dyn Fn(Id<InteractionMarker>, &InteractionResponse) -> Option<Interaction> + Send + Sync>>,
        handler: Option<std::sync::Weak<InteractionHandler>>
    }

    pub struct FakeInteraction<'a>(&'a FakeClient);

    impl FakeClient {
        pub fn interaction<'a>(&'a self, _: Id<ApplicationMarker>) -> FakeInteraction<'a> {
            FakeInteraction(self)
        }
    }

    impl<'a> FakeInteraction<'a> {
        pub fn create_response<'b>(&'b self, inter_id: Id<InteractionMarker>, token: &'b str, resp: &'b InteractionResponse) -> Pin<Box<dyn Future<Output=Result<(), twilight_http::Error>> + Send + 'b>> {
            Box::pin(self.create_response_real(inter_id, token, resp))
        }
        async fn create_response_real(&self, inter_id: Id<InteractionMarker>, token: &str, resp: &InteractionResponse) -> Result<(), twilight_http::Error> {
            println!("create_response invoked");
            if let Some(f) = self.0.reply_fn.as_ref() {
                println!("found a function");
                if let Some(inter) = f(inter_id, &resp) {
                    println!("injecting second future");
                    let handler = self.0.handler.clone().unwrap().upgrade().unwrap();
                    tokio::spawn(handler.handle(inter));
                }
            }
            self.0.interaction_responses.lock().await.push((inter_id, resp.clone()));
            Ok(())
        }
        pub async fn update_response(&self, token: &str, resp: &InteractionResponse) -> Result<(), twilight_http::Error> {
            Ok(())
        }
    }

    #[test]
    fn test_ping_gets_pong() {
        let handler = Arc::new(InteractionHandler::new(Default::default(), &COMMANDS));
        let h = handler.clone();
        tokio::runtime::Builder::new_current_thread().build().unwrap().block_on(async {
            #[allow(deprecated)]
            h.handle(Interaction {
                app_permissions: None,
                application_id: Id::new(1),
                channel: None,
                channel_id: None,
                data: None,
                guild_id: None,
                guild_locale: None,
                id: Id::new(1),
                kind: InteractionType::Ping,
                locale: None,
                member: None,
                message: None,
                token: "".into(),
                user: None,
            }).await;
        });
        let handler = Arc::into_inner(handler).expect("outsanding references to the handler");
        let client = Arc::into_inner(handler.client).expect("outsanding references to the client");

        let resps = client.interaction_responses.into_inner();
        assert_eq!(resps.len(), 1);
        assert_eq!(resps[0].1.kind, InteractionResponseType::Pong);
    }

    #[test]
    fn test_command_response() {
        let handler = Arc::new(InteractionHandler::new(Default::default(), &COMMANDS));
        let h = handler.clone();
        tokio::runtime::Builder::new_current_thread().build().unwrap().block_on(async {
            #[allow(deprecated)]
            h.handle(Interaction {
                app_permissions: None,
                application_id: Id::new(1),
                channel: None,
                channel_id: None,
                data: Some(InteractionData::ApplicationCommand(Box::new(CommandData {
                    guild_id: None,
                    id: Id::new(1),
                    kind: CommandType::ChatInput,
                    name: "hello".into(),
                    options: vec![],
                    resolved: None,
                    target_id: None,
                }))),
                guild_id: None,
                guild_locale: None,
                id: Id::new(1),
                kind: InteractionType::ApplicationCommand,
                locale: None,
                member: None,
                message: None,
                token: "".into(),
                user: None,
            }).await;
        });
        let handler = Arc::into_inner(handler).expect("outsanding references to the handler");
        let client = Arc::into_inner(handler.client).expect("outsanding references to the client");

        let resps = client.interaction_responses.into_inner();
        assert_eq!(resps.len(), 1);
        assert_eq!(resps[0].1.kind, InteractionResponseType::ChannelMessageWithSource);
    }
    #[test]
    fn test_command_error() {
        let handler = Arc::new(InteractionHandler::new(Default::default(), &COMMANDS));
        let rt = tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap();
        let _a = rt.enter();
        let h = handler.clone();
        let _ = rt.block_on(tokio::time::timeout(Duration::from_secs(5), async {
            #[allow(deprecated)]
            h.handle(Interaction {
                app_permissions: None,
                application_id: Id::new(1),
                channel: None,
                channel_id: None,
                data: Some(InteractionData::ApplicationCommand(Box::new(CommandData {
                    guild_id: None,
                    id: Id::new(1),
                    kind: CommandType::ChatInput,
                    name: "error".into(),
                    options: vec![],
                    resolved: None,
                    target_id: None,
                }))),
                guild_id: None,
                guild_locale: None,
                id: Id::new(1),
                kind: InteractionType::ApplicationCommand,
                locale: None,
                member: None,
                message: None,
                token: "".into(),
                user: None,
            }).await;
        }));
        let handler = Arc::into_inner(handler).expect("outsanding references to the handler");
        let client = Arc::into_inner(handler.client).expect("outsanding references to the client");

        let resps = client.interaction_responses.into_inner();
        assert_eq!(resps.len(), 1);
        assert_eq!(resps[0].1.kind, InteractionResponseType::ChannelMessageWithSource);
    }
    #[test]
    fn test_modal_show() {
        let mut handler = Arc::new(InteractionHandler::new(Default::default(), &COMMANDS));
        let handler_weak = Arc::downgrade(&handler);
        let handler_mut = unsafe {Arc::get_mut_unchecked(&mut handler)};
        let client_mut = Arc::get_mut(&mut handler_mut.client).unwrap();
        client_mut.handler = Some(handler_weak);
        client_mut.reply_fn = Some(Box::new(|id, resp| {
            println!("replying to: {}", id);
            if id == Id::new(1001) {
                #[allow(deprecated)]
                Some(Interaction {
                    app_permissions: None,
                    application_id: Id::new(1),
                    channel: None,
                    channel_id: None,
                    data: Some(InteractionData::ModalSubmit(ModalInteractionData {
                        custom_id: resp.data.as_ref().expect("command response had no dtaa").custom_id.clone().expect("app did not provide a custom ID on the modal"),
                        components: vec![],
                    })),
                    guild_id: None,
                    guild_locale: None,
                    id: Id::new(1),
                    kind: InteractionType::ModalSubmit,
                    locale: None,
                    member: None,
                    message: None,
                    token: "yeet lol".into(),
                    user: None,
                })
            } else {
                None
            }
        }));
        let rt = tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap();
        let _a = rt.enter();
        let h = handler.clone();
        let _ = rt.block_on(tokio::time::timeout(Duration::from_secs(5), async {
            #[allow(deprecated)]
            h.handle(Interaction {
                app_permissions: None,
                application_id: Id::new(1),
                channel: None,
                channel_id: None,
                data: Some(InteractionData::ApplicationCommand(Box::new(CommandData {
                    guild_id: None,
                    id: Id::new(1),
                    kind: CommandType::ChatInput,
                    name: "modal".into(),
                    options: vec![],
                    resolved: None,
                    target_id: None,
                }))),
                guild_id: None,
                guild_locale: None,
                id: Id::new(1001),
                kind: InteractionType::ApplicationCommand,
                locale: None,
                member: None,
                message: None,
                token: "".into(),
                user: None,
            }).await;
            let mut resps = handler.client.interaction_responses.try_lock().expect("application held onto the lock");
            let resp = resps.pop().expect("application did not reply to modal command");
            println!("yo");
            std::mem::drop(resps);

        }));
        let handler = Arc::into_inner(handler).expect("outsanding references to the handler");
        let client = Arc::into_inner(handler.client).expect("outsanding references to the client");

        let resps = client.interaction_responses.into_inner();
        assert_eq!(resps.len(), 1);
        if resps[0].1.kind != InteractionResponseType::ChannelMessageWithSource {
            panic!("expected an error message, got {:?}", resps[0].1);
        }
    }
}
