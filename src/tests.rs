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
    let r = h.show_modal(&inter, String::from("yeet"), vec![], "showin the modal").await.unwrap();
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
        if let Some(f) = self.0.reply_fn.as_ref() {
            if let Some(inter) = f(inter_id, &resp) {
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
    // SAFETY: no weak references are either dereferenced or borrowed before handler_mut is
    // dropped.  Ownership of handler_weak is transferred into client_mut.  No borrow occurs,
    // therefore safe.
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
    }));
    let handler = Arc::into_inner(handler).expect("outsanding references to the handler");
    let client = Arc::into_inner(handler.client).expect("outsanding references to the client");

    let resps = client.interaction_responses.into_inner();
    if resps.len() != 2 {
        panic!("expected two entries, found {}: {:?}", resps.len(), resps);
    }
    if resps[1].1.kind != InteractionResponseType::ChannelMessageWithSource {
        panic!("expected an error message, got {:?}", resps[0].1);
    }
}

