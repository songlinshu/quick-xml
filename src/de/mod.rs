//! Serde `Deserializer` module

mod errors;
mod escape;
mod map;
mod seq;
mod var;

pub use self::errors::DeError;
use crate::{
    events::{BytesStart, BytesText, Event},
    Reader,
};
use serde::de::{self, DeserializeOwned};
use serde::forward_to_deserialize_any;
use std::io::BufRead;

const INNER_VALUE: &str = "$value";

/// An xml deserializer
pub struct Deserializer<R: BufRead> {
    reader: Reader<R>,
    peek: Option<Event<'static>>,
    peek_text: Option<BytesText<'static>>,
    depth: usize,
}

/// Deserialize a xml string
pub fn from_str<T: DeserializeOwned>(s: &str) -> Result<T, DeError> {
    from_reader(s.as_bytes())
}

/// Deserialize from a reader
pub fn from_reader<R: BufRead, T: DeserializeOwned>(reader: R) -> Result<T, DeError> {
    let mut de = Deserializer::from_reader(reader);
    T::deserialize(&mut de)
}

impl<R: BufRead> Deserializer<R> {
    /// Get a new deserializer
    pub fn new(reader: Reader<R>) -> Self {
        Deserializer {
            reader,
            peek: None,
            peek_text: None,
            depth: 0,
        }
    }

    /// Get a new deserializer from a regular BufRead
    pub fn from_reader(reader: R) -> Self {
        let mut reader = Reader::from_reader(reader);
        reader
            .expand_empty_elements(true)
            .check_end_names(true)
            .trim_text(true);
        Self::new(reader)
    }

    fn peek(&mut self) -> Result<Option<&Event<'static>>, DeError> {
        if self.peek.is_none() {
            let mut buf = Vec::new();
            self.peek = Some(self.reader.read_event(&mut buf)?.into_owned());
        }
        Ok(self.peek.as_ref())
    }

    fn next<'a>(&mut self, buf: &'a mut Vec<u8>) -> Result<Event<'a>, DeError> {
        if let Some(e) = self.peek.take() {
            return Ok(e);
        }
        Ok(self.reader.read_event(buf)?)
    }

    fn next_text(&mut self) -> Result<BytesText<'static>, DeError> {
        if let Some(t) = self.peek_text.take() {
            return Ok(t);
        }
        let mut buf = Vec::new();
        let depth = self.depth;
        loop {
            let e = self.next(&mut buf)?;
            match e {
                Event::Start(e) => {
                    if self.depth == depth + 1 {
                        self.reader.read_to_end(e.name(), &mut Vec::new())?;
                    }
                    self.depth += 1;
                }
                Event::End(_) => {
                    if self.depth == depth + 1 {
                        self.depth -= 1;
                        return Ok(BytesText::from_escaped(&[] as &'static [u8]));
                    }
                    return Err(DeError::End);
                }
                Event::Text(e) => {
                    self.read_to_depth(depth)?;
                    return Ok(e.into_owned());
                }
                Event::Eof => return Err(DeError::Eof),
                _ => buf.clear(),
            }
        }
    }

    fn next_start(&mut self) -> Result<Option<BytesStart<'static>>, DeError> {
        let mut buf = Vec::new();
        if let Event::Start(e) = self.next(&mut buf)? {
            Ok(Some(e.into_owned()))
        } else {
            Ok(None)
        }
    }

    fn read_to_depth(&mut self, depth: usize) -> Result<(), DeError> {
        if self.depth > depth {
            let mut buf = Vec::new();
            while self.depth != depth {
                match self.next(&mut buf)? {
                    Event::Start(_) => self.depth += 1,
                    Event::End(_) => self.depth -= 1,
                    Event::Eof => return Err(DeError::Eof),
                    _ => buf.clear(),
                }
            }
        }
        Ok(())
    }

    fn read_to_end(&mut self, name: &[u8]) -> Result<(), DeError> {
        // don't use self.reader.read_to_end because there may be some peeked item
        let mut buf = Vec::new();
        let mut depth = 1;
        loop {
            match self.next(&mut buf)? {
                Event::End(e) => {
                    depth -= 1;
                    if depth == 0 && e.name() == name {
                        break;
                    }
                }
                Event::Start(_) => depth += 1,
                Event::Eof => return Err(DeError::Eof),
                _ => (),
            }
        }
        Ok(())
    }
}

macro_rules! deserialize_type {
    ($deserialize:ident => $visit:ident) => {
        fn $deserialize<V: de::Visitor<'de>>(self, visitor: V) -> Result<V::Value, DeError> {
            let txt = self.next_text()?;

            #[cfg(not(feature = "encoding"))]
            let value = self.reader.decode(&*txt)?.parse()?;

            #[cfg(feature = "encoding")]
            let value = self.reader.decode(&*txt).parse()?;

            visitor.$visit(value)
        }
    }
}

impl<'de, 'a, R: BufRead> de::Deserializer<'de> for &'a mut Deserializer<R> {
    type Error = DeError;

    forward_to_deserialize_any! { newtype_struct identifier }

    fn deserialize_struct<V: de::Visitor<'de>>(
        self,
        _name: &'static str,
        fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, DeError> {
        if let Some(e) = self.next_start()? {
            let name = e.name().to_vec();
            let inner_value = fields.contains(&INNER_VALUE);
            let map = map::MapAccess::new(self, e, self.reader.decoder(), inner_value)?;
            let value = visitor.visit_map(map)?;
            self.read_to_end(&name)?;
            Ok(value)
        } else {
            Err(DeError::Start)
        }
    }

    deserialize_type!(deserialize_i8 => visit_i8);
    deserialize_type!(deserialize_i16 => visit_i16);
    deserialize_type!(deserialize_i32 => visit_i32);
    deserialize_type!(deserialize_i64 => visit_i64);
    deserialize_type!(deserialize_u8 => visit_u8);
    deserialize_type!(deserialize_u16 => visit_u16);
    deserialize_type!(deserialize_u32 => visit_u32);
    deserialize_type!(deserialize_u64 => visit_u64);
    deserialize_type!(deserialize_f32 => visit_f32);
    deserialize_type!(deserialize_f64 => visit_f64);

    fn deserialize_bool<V: de::Visitor<'de>>(self, visitor: V) -> Result<V::Value, DeError> {
        let txt = self.next_text()?;

        #[cfg(feature = "encoding")]
        {
            #[cfg(feature = "encoding")]
            let value = self.reader.decode(&*txt);

            match value.as_ref() {
                "true" | "1" | "True" | "TRUE" | "t" | "Yes" | "YES" | "yes" | "y" => {
                    visitor.visit_bool(true)
                }
                "false" | "0" | "False" | "FALSE" | "f" | "No" | "NO" | "no" | "n" => {
                    visitor.visit_bool(false)
                }
                _ => Err(DeError::InvalidBoolean(value.into())),
            }
        }

        #[cfg(not(feature = "encoding"))]
        {
            match txt.as_ref() {
                b"true" | b"1" | b"True" | b"TRUE" | b"t" | b"Yes" | b"YES" | b"yes" | b"y" => {
                    visitor.visit_bool(true)
                }
                b"false" | b"0" | b"False" | b"FALSE" | b"f" | b"No" | b"NO" | b"no" | b"n" => {
                    visitor.visit_bool(false)
                }
                e => Err(DeError::InvalidBoolean(self.reader.decode(e)?.into())),
            }
        }
    }

    fn deserialize_string<V: de::Visitor<'de>>(self, visitor: V) -> Result<V::Value, DeError> {
        let value = self.next_text()?.unescape_and_decode(&self.reader)?;
        visitor.visit_string(value)
    }

    fn deserialize_char<V: de::Visitor<'de>>(self, visitor: V) -> Result<V::Value, DeError> {
        self.deserialize_string(visitor)
    }

    fn deserialize_str<V: de::Visitor<'de>>(self, visitor: V) -> Result<V::Value, DeError> {
        self.deserialize_string(visitor)
    }

    fn deserialize_bytes<V: de::Visitor<'de>>(self, visitor: V) -> Result<V::Value, DeError> {
        self.deserialize_string(visitor)
    }

    fn deserialize_byte_buf<V: de::Visitor<'de>>(self, visitor: V) -> Result<V::Value, DeError> {
        self.deserialize_string(visitor)
    }

    fn deserialize_unit<V: de::Visitor<'de>>(self, visitor: V) -> Result<V::Value, DeError> {
        let value = self.next_text()?;
        if value.is_empty() {
            visitor.visit_unit()
        } else {
            Err(DeError::InvalidUnit(
                value.unescape_and_decode(&self.reader)?,
            ))
        }
    }

    fn deserialize_unit_struct<V: de::Visitor<'de>>(
        self,
        _name: &'static str,
        visitor: V,
    ) -> Result<V::Value, DeError> {
        self.deserialize_unit(visitor)
    }

    fn deserialize_tuple<V: de::Visitor<'de>>(
        self,
        len: usize,
        visitor: V,
    ) -> Result<V::Value, DeError> {
        visitor.visit_seq(seq::SeqAccess::new(self, Some(len)))
    }

    fn deserialize_tuple_struct<V: de::Visitor<'de>>(
        self,
        _name: &'static str,
        len: usize,
        visitor: V,
    ) -> Result<V::Value, DeError> {
        self.deserialize_tuple(len, visitor)
    }

    fn deserialize_enum<V: de::Visitor<'de>>(
        self,
        _name: &'static str,
        _variants: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, DeError> {
        visitor.visit_enum(var::EnumAccess::new(self))
    }

    fn deserialize_seq<V: de::Visitor<'de>>(self, visitor: V) -> Result<V::Value, DeError> {
        visitor.visit_seq(seq::SeqAccess::new(self, None))
    }

    fn deserialize_map<V: de::Visitor<'de>>(self, visitor: V) -> Result<V::Value, DeError> {
        self.deserialize_struct("", &[], visitor)
    }

    fn deserialize_option<V: de::Visitor<'de>>(self, visitor: V) -> Result<V::Value, DeError> {
        let value = self.next_text()?;
        if value.is_empty() {
            visitor.visit_none()
        } else {
            self.peek_text = Some(value);
            visitor.visit_some(self)
        }
    }

    fn deserialize_ignored_any<V: de::Visitor<'de>>(self, visitor: V) -> Result<V::Value, DeError> {
        let depth = self.depth;
        let mut buf = Vec::new();
        let _ = self.next(&mut buf)?;
        self.read_to_depth(depth)?;
        visitor.visit_unit()
    }

    fn deserialize_any<V: de::Visitor<'de>>(self, visitor: V) -> Result<V::Value, DeError> {
        match self.peek()?.ok_or(DeError::Eof)? {
            Event::Start(_) => self.deserialize_map(visitor),
            Event::End(_) => self.deserialize_unit(visitor),
            _ => self.deserialize_string(visitor),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Debug, Deserialize, PartialEq)]
    struct Item {
        name: String,
        source: String,
    }

    #[test]
    fn simple_struct_from_attributes() {
        let s = r##"
	    <item name="hello" source="world.rs" />
	"##;

        let item: Item = from_str(s).unwrap();

        assert_eq!(
            item,
            Item {
                name: "hello".to_string(),
                source: "world.rs".to_string(),
            }
        );
    }

    #[test]
    fn multiple_roots_attributes() {
        let s = r##"
	    <item name="hello" source="world.rs" />
	    <item name="hello" source="world.rs" />
	"##;

        let item: Vec<Item> = from_str(s).unwrap();

        assert_eq!(
            item,
            vec![
                Item {
                    name: "hello".to_string(),
                    source: "world.rs".to_string(),
                },
                Item {
                    name: "hello".to_string(),
                    source: "world.rs".to_string(),
                },
            ]
        );
    }

    #[test]
    fn simple_struct_from_attribute_and_child() {
        let s = r##"
	    <item name="hello">
		<source>world.rs</source>
	    </item>
	"##;

        let item: Item = from_str(s).unwrap();

        assert_eq!(
            item,
            Item {
                name: "hello".to_string(),
                source: "world.rs".to_string(),
            }
        );
    }

    #[derive(Debug, Deserialize, PartialEq)]
    struct Project {
        name: String,

        #[serde(rename = "item", default)]
        items: Vec<Item>,
    }

    #[test]
    fn nested_collection() {
        let s = r##"
	    <project name="my_project">
		<item name="hello1" source="world1.rs" />
		<item name="hello2" source="world2.rs" />
	    </project>
	"##;

        let project: Project = from_str(s).unwrap();

        assert_eq!(
            project,
            Project {
                name: "my_project".to_string(),
                items: vec![
                    Item {
                        name: "hello1".to_string(),
                        source: "world1.rs".to_string(),
                    },
                    Item {
                        name: "hello2".to_string(),
                        source: "world2.rs".to_string(),
                    },
                ],
            }
        );
    }

    #[derive(Debug, Deserialize, PartialEq)]
    enum MyEnum {
        A(String),
        B { name: String, flag: bool },
        C,
    }

    #[derive(Debug, Deserialize, PartialEq)]
    struct MyEnums {
        #[serde(rename = "$value")]
        items: Vec<MyEnum>,
    }

    #[test]
    fn collection_of_enums() {
        let s = r##"
	    <enums>
		<A>test</A>
		<B name="hello" flag="t" />
		<C />
	    </enums>
	"##;

        let project: MyEnums = from_str(s).unwrap();

        assert_eq!(
            project,
            MyEnums {
                items: vec![
                    MyEnum::A("test".to_string()),
                    MyEnum::B {
                        name: "hello".to_string(),
                        flag: true,
                    },
                    MyEnum::C,
                ],
            }
        );
    }
}
