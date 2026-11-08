
/// RGB controller device type.
///
/// See [Open SDK documentation](https://gitlab.com/CalcProgrammer1/OpenRGB/-/wikis/OpenRGB-SDK-Documentation) for more information.
#[derive(Eq, PartialEq, Debug, Copy, Clone, Hash)]
#[repr(u32)]
pub enum DeviceType {
    /// Motherboard.
    Motherboard = 0,
    /// DRAM
    DRam = 1,
    /// GPU
    Gpu = 2,
    /// Cooler
    Cooler = 3,
    /// LED strip
    LEDStrip = 4,
    /// Keyboard
    Keyboard = 5,
    /// Mouse
    Mouse = 6,
    /// Mouse mat
    MouseMat = 7,
    /// Headset
    Headset = 8,
    /// Headset stand
    HeadsetStand = 9,
    /// Gamepad
    Gamepad = 10,
    /// Light
    Light = 11,
    /// Speaker
    Speaker = 12,
    /// Virtual
    Virtual = 13,
    /// Unknown
    Unknown = 21,
    /// OpenRGB device category.
    Storage = 14,
    /// OpenRGB device category.
    Case = 15,
    /// OpenRGB device category.
    Microphone = 16,
    /// OpenRGB device category.
    Accessory = 17,
    /// OpenRGB device category.
    Keypad = 18,
    /// OpenRGB device category.
    Display = 19,
    /// OpenRGB device category.
    Console = 20,
    /// Future wire value, retained for discovery.
    Unrecognized(u32) = 4294967295,
}

impl crate::protocol::DeserFromBuf for DeviceType {
    fn deserialize(buf: &mut crate::ReceivedMessage<'_>) -> crate::OpenRgbResult<Self> {
        Ok(match buf.read_u32()? {
            0 => Self::Motherboard,
            1 => Self::DRam,
            2 => Self::Gpu,
            3 => Self::Cooler,
            4 => Self::LEDStrip,
            5 => Self::Keyboard,
            6 => Self::Mouse,
            7 => Self::MouseMat,
            8 => Self::Headset,
            9 => Self::HeadsetStand,
            10 => Self::Gamepad,
            11 => Self::Light,
            12 => Self::Speaker,
            13 => Self::Virtual,
            21 => Self::Unknown,
            14 => Self::Storage,
            15 => Self::Case,
            16 => Self::Microphone,
            17 => Self::Accessory,
            18 => Self::Keypad,
            19 => Self::Display,
            20 => Self::Console,
            other => Self::Unrecognized(other),
        })
    }
}
impl crate::protocol::SerToBuf for DeviceType {
    fn serialize(&self, buf: &mut crate::WriteMessage) -> crate::OpenRgbResult<()> {
        let value = match self {
            Self::Motherboard => 0,
            Self::DRam => 1,
            Self::Gpu => 2,
            Self::Cooler => 3,
            Self::LEDStrip => 4,
            Self::Keyboard => 5,
            Self::Mouse => 6,
            Self::MouseMat => 7,
            Self::Headset => 8,
            Self::HeadsetStand => 9,
            Self::Gamepad => 10,
            Self::Light => 11,
            Self::Speaker => 12,
            Self::Virtual => 13,
            Self::Unknown => 21,
            Self::Storage => 14,
            Self::Case => 15,
            Self::Microphone => 16,
            Self::Accessory => 17,
            Self::Keypad => 18,
            Self::Display => 19,
            Self::Console => 20,
            Self::Unrecognized(other) => *other,
        };
        buf.write_u32(value);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::error::Error;

    use crate::{WriteMessage, data::DeviceType};

    #[tokio::test]
    async fn test_read_001() -> Result<(), Box<dyn Error>> {
        let mut buf = WriteMessage::new(crate::DEFAULT_PROTOCOL);
        let mut msg = buf.push_value(3_u32)?.to_received_msg();

        assert_eq!(msg.read_value::<DeviceType>()?, DeviceType::Cooler);
        Ok(())
    }

    #[tokio::test]
    async fn test_write_001() -> Result<(), Box<dyn Error>> {
        let mut buf = WriteMessage::new(crate::DEFAULT_PROTOCOL);
        let mut msg = buf.push_value(DeviceType::Cooler)?.to_received_msg();
        assert_eq!(msg.read_value::<u32>()?, 3);
        Ok(())
    }
}
