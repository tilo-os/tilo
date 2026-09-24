//! Nonallocating traversal of concrete retained DTO Serialize implementations.
use super::*;
use serde::ser::*;
#[derive(Default)]
struct Measure {
    sum: usize,
    largest: usize,
}
pub(super) fn bytes<T: Serialize>(v: &T) -> Result<usize> {
    let mut m = Measure::default();
    v.serialize(&mut m)?;
    Ok(m.sum)
}
pub(super) fn largest<T: Serialize>(v: &T) -> Result<usize> {
    let mut m = Measure::default();
    v.serialize(&mut m)?;
    Ok(m.largest)
}
impl<'a> Serializer for &'a mut Measure {
    type Ok = ();
    type Error = Capacity;
    type SerializeSeq = Self;
    type SerializeTuple = Self;
    type SerializeTupleStruct = Self;
    type SerializeTupleVariant = Self;
    type SerializeMap = Self;
    type SerializeStruct = Self;
    type SerializeStructVariant = Self;
    fn serialize_bool(self, _: bool) -> Result<()> {
        Ok(())
    }
    fn serialize_i8(self, _: i8) -> Result<()> {
        Ok(())
    }
    fn serialize_i16(self, _: i16) -> Result<()> {
        Ok(())
    }
    fn serialize_i32(self, _: i32) -> Result<()> {
        Ok(())
    }
    fn serialize_i64(self, _: i64) -> Result<()> {
        Ok(())
    }
    fn serialize_i128(self, _: i128) -> Result<()> {
        Ok(())
    }
    fn serialize_u8(self, _: u8) -> Result<()> {
        Ok(())
    }
    fn serialize_u16(self, _: u16) -> Result<()> {
        Ok(())
    }
    fn serialize_u32(self, _: u32) -> Result<()> {
        Ok(())
    }
    fn serialize_u64(self, _: u64) -> Result<()> {
        Ok(())
    }
    fn serialize_u128(self, _: u128) -> Result<()> {
        Ok(())
    }
    fn serialize_f32(self, _: f32) -> Result<()> {
        Ok(())
    }
    fn serialize_f64(self, _: f64) -> Result<()> {
        Ok(())
    }
    fn serialize_char(self, _: char) -> Result<()> {
        Ok(())
    }
    fn serialize_str(self, v: &str) -> Result<()> {
        self.sum = sum(&[self.sum, v.len()])?;
        self.largest = self.largest.max(v.len());
        Ok(())
    }
    fn serialize_bytes(self, _: &[u8]) -> Result<()> {
        Ok(())
    }
    fn serialize_none(self) -> Result<()> {
        Ok(())
    }
    fn serialize_some<T: ?Sized + Serialize>(self, v: &T) -> Result<()> {
        v.serialize(self)
    }
    fn serialize_unit(self) -> Result<()> {
        Ok(())
    }
    fn serialize_unit_struct(self, _: &'static str) -> Result<()> {
        Ok(())
    }
    fn serialize_unit_variant(self, _: &'static str, _: u32, _: &'static str) -> Result<()> {
        Ok(())
    }
    fn serialize_newtype_struct<T: ?Sized + Serialize>(self, _: &'static str, v: &T) -> Result<()> {
        v.serialize(self)
    }
    fn serialize_newtype_variant<T: ?Sized + Serialize>(
        self,
        _: &'static str,
        _: u32,
        _: &'static str,
        v: &T,
    ) -> Result<()> {
        v.serialize(self)
    }
    fn serialize_seq(self, _: Option<usize>) -> Result<Self> {
        Ok(self)
    }
    fn serialize_tuple(self, _: usize) -> Result<Self> {
        Ok(self)
    }
    fn serialize_tuple_struct(self, _: &'static str, _: usize) -> Result<Self> {
        Ok(self)
    }
    fn serialize_tuple_variant(
        self,
        _: &'static str,
        _: u32,
        _: &'static str,
        _: usize,
    ) -> Result<Self> {
        Ok(self)
    }
    fn serialize_map(self, _: Option<usize>) -> Result<Self> {
        Ok(self)
    }
    fn serialize_struct(self, _: &'static str, _: usize) -> Result<Self> {
        Ok(self)
    }
    fn serialize_struct_variant(
        self,
        _: &'static str,
        _: u32,
        _: &'static str,
        _: usize,
    ) -> Result<Self> {
        Ok(self)
    }
    fn collect_str<T: ?Sized + std::fmt::Display>(self, v: &T) -> Result<()> {
        struct C(usize);
        impl std::fmt::Write for C {
            fn write_str(&mut self, s: &str) -> std::fmt::Result {
                self.0 = self.0.checked_add(s.len()).ok_or(std::fmt::Error)?;
                Ok(())
            }
        }
        let mut c = C(0);
        std::fmt::write(&mut c, format_args!("{v}")).map_err(|_| Capacity)?;
        self.sum = sum(&[self.sum, c.0])?;
        self.largest = self.largest.max(c.0);
        Ok(())
    }
}
impl SerializeSeq for &mut Measure {
    type Ok = ();
    type Error = Capacity;
    fn serialize_element<T: ?Sized + Serialize>(&mut self, v: &T) -> Result<()> {
        v.serialize(&mut **self)
    }
    fn end(self) -> Result<()> {
        Ok(())
    }
}
impl SerializeTuple for &mut Measure {
    type Ok = ();
    type Error = Capacity;
    fn serialize_element<T: ?Sized + Serialize>(&mut self, v: &T) -> Result<()> {
        v.serialize(&mut **self)
    }
    fn end(self) -> Result<()> {
        Ok(())
    }
}
impl SerializeTupleStruct for &mut Measure {
    type Ok = ();
    type Error = Capacity;
    fn serialize_field<T: ?Sized + Serialize>(&mut self, v: &T) -> Result<()> {
        v.serialize(&mut **self)
    }
    fn end(self) -> Result<()> {
        Ok(())
    }
}
impl SerializeTupleVariant for &mut Measure {
    type Ok = ();
    type Error = Capacity;
    fn serialize_field<T: ?Sized + Serialize>(&mut self, v: &T) -> Result<()> {
        v.serialize(&mut **self)
    }
    fn end(self) -> Result<()> {
        Ok(())
    }
}
impl SerializeStruct for &mut Measure {
    type Ok = ();
    type Error = Capacity;
    fn serialize_field<T: ?Sized + Serialize>(&mut self, _: &'static str, v: &T) -> Result<()> {
        v.serialize(&mut **self)
    }
    fn end(self) -> Result<()> {
        Ok(())
    }
}
impl SerializeStructVariant for &mut Measure {
    type Ok = ();
    type Error = Capacity;
    fn serialize_field<T: ?Sized + Serialize>(&mut self, _: &'static str, v: &T) -> Result<()> {
        v.serialize(&mut **self)
    }
    fn end(self) -> Result<()> {
        Ok(())
    }
}
impl SerializeMap for &mut Measure {
    type Ok = ();
    type Error = Capacity;
    fn serialize_key<T: ?Sized + Serialize>(&mut self, v: &T) -> Result<()> {
        v.serialize(&mut **self)
    }
    fn serialize_value<T: ?Sized + Serialize>(&mut self, v: &T) -> Result<()> {
        v.serialize(&mut **self)
    }
    fn end(self) -> Result<()> {
        Ok(())
    }
}
