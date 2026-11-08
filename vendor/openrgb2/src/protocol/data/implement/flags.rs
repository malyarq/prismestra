use flagset::FlagSet;

use crate::{DeserFromBuf, ReceivedMessage, SerToBuf};

impl<T> DeserFromBuf for FlagSet<T>
where
    T: flagset::Flags<Type = u32>,
{
    fn deserialize(buf: &mut ReceivedMessage<'_>) -> crate::OpenRgbResult<Self> {
        let value = buf.read_value::<u32>()?;
        Ok(FlagSet::<T>::new_truncated(value))
    }
}

impl<T> SerToBuf for FlagSet<T>
where
    T: flagset::Flags<Type = u32>,
{
    fn serialize(&self, buf: &mut crate::WriteMessage) -> crate::OpenRgbResult<()> {
        buf.write_u32(self.bits());
        Ok(())
    }
}
