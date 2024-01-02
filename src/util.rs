use serde::de::{Deserializer, MapAccess, DeserializeSeed};
use serde::de::value::{MapAccessDeserializer, StringDeserializer, UnitDeserializer};
use twilight_model::application::interaction::modal::{ModalInteractionData,ModalInteractionDataActionRow};
use twilight_model::channel::message::component::{TextInput,TextInputStyle};

struct ModalUnwrapper {
    inner: std::vec::IntoIter<ModalInteractionDataActionRow>,
    value: Option<Option<String>>,
}

pub struct ModalError(String);

impl std::fmt::Debug for ModalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}
impl std::fmt::Display for ModalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl std::error::Error for ModalError {}

impl serde::de::Error for ModalError {
    fn custom<T>(val: T) -> Self where T: std::fmt::Display {
        Self(val.to_string())
    }
}

impl<'de> MapAccess<'de> for ModalUnwrapper {
    type Error = ModalError;
    fn next_key_seed<V>(&mut self, visitor:V) -> Result<Option<V::Value>, Self::Error> where V: DeserializeSeed<'de> {
        debug_assert!(self.value.is_none(), "next_key_seed() called twice in a row");
        self.inner.next().map(move |val| {
            let data = val.components.into_iter().next().expect("Discord returned an empty action row???");
            let key = data.custom_id;
            self.value = Some(data.value);
            visitor.deserialize(StringDeserializer::new(key))
        }).transpose()
    }
    fn next_value_seed<V>(&mut self, visitor:V) -> Result<V::Value, Self::Error> where V: DeserializeSeed<'de> {
        let val = self.value.take().expect("next_value_seed() was called twice in a row");
        match val {
            Some(s) => visitor.deserialize(StringDeserializer::new(s)),
            None    => visitor.deserialize(UnitDeserializer::new())
        }
    }
}

impl ModalUnwrapper {
    pub fn new(v: Vec<ModalInteractionDataActionRow>) -> Self {
        Self {
            inner: v.into_iter(),
            value: None,
        }
    }
}

pub fn unwrap_modal<T: for<'a> serde::Deserialize<'a>>(modal: ModalInteractionData) -> Result<T, ModalError> {
    T::deserialize(MapAccessDeserializer::new(ModalUnwrapper::new(modal.components)))
}

pub struct TextInputBuilder {
    inner: TextInput,
}

impl TextInputBuilder {
    pub fn new(label: impl Into<String>, custom_id: impl Into<String>, style: TextInputStyle) -> Self {
        Self{inner: TextInput {
            custom_id: custom_id.into(),
            label: label.into(),
            style,
            max_length: None,
            min_length: None,
            placeholder: None,
            required: None,
            value: None,

        }}
    }
    pub fn build(self) -> TextInput {
        self.inner
    }

    pub fn max_length(mut self, max_length: u16) -> Self {
        self.inner.max_length = Some(max_length);
        self
    }

    pub fn min_length(mut self, min_length: u16) -> Self {
        self.inner.min_length = Some(min_length);
        self
    }
    pub fn required(mut self, required: bool) -> Self {
        self.inner.required = Some(required);
        self
    }

    pub fn placeholder(mut self, placeholder: impl Into<String>) -> Self {
        self.inner.placeholder = Some(placeholder.into());
        self
    }
    pub fn value(mut self, value: impl Into<String>) -> Self {
        self.inner.value = Some(value.into());
        self
    }

}

#[macro_export]
macro_rules! components  {
    [$($item:expr),+] => {
         vec![$(::twilight_model::channel::message::component::Component::from($item)),*]
    }
}

#[macro_export]
macro_rules! action_row  {
    [$($item:expr),*] => {
         ::twilight_model::channel::message::component::ActionRow{ components: vec![$($item.into()),*]}
    }
}

#[macro_export]
macro_rules! _disabled_helper {
    ($val: expr) => {$val};
    () => {false};
}

#[macro_export]
macro_rules! _option_helper {
    ($val: expr) => {Some($val.into())};
    () => {None};
}

#[macro_export]
macro_rules! button  {
    (style: $style:ident $(,label: $label: expr)? $(,emoji: $emoji: expr)? $(,disabled: $disabled:expr)? $(, id: $id:expr)?) => { 
        ::twilight_model::channel::message::component::Button {
            disabled: $crate::_disabled_helper!($($disabled)?),
            label: $crate::_option_helper!($($label)?),
            emoji: $crate::_option_helper!($($emoji)?),
            custom_id: $crate::_option_helper!($($id)?),
            style: ::twilight_model::channel::message::component::ButtonStyle::$style,
            url: None,
        }
    }
}

#[macro_export]
macro_rules! link_button  {
    (url: $url:expr $(,label: $label: expr)? $(,emoji: $emoji: expr)? $(,disabled: $disabled:expr)?) => { 
        ::twilight_model::channel::message::component::Button {
            disabled: $crate::_disabled_helper!($($disabled)?),
            label: $crate::_option_helper!($($label)?),
            emoji: $crate::_option_helper!($($emoji)?),
            custom_id: None,
            style: ::twilight_model::channel::message::component::ButtonStyle::$style,
            url: Some($url.into()),
        }
    }
}

#[macro_export]
macro_rules! response {
    (ephemeral; $client: expr, $inter: expr, $($args:tt)*) => {
        $client.send_response(&$inter, InteractionResponseDataBuilder::new().content(format!($($args)*)).flags(MessageFlags::EPHEMERAL).build()).await?;
    };
    ($client: expr, $inter: expr, $($args:tt)*) => {
        $client.send_response(&$inter, InteractionResponseDataBuilder::new().content(format!($($args)*)).build()).await?;
    };
}
