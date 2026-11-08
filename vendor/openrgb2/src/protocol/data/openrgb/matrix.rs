use crate::{DeserFromBuf, RawSlice, ReceivedMessage, SerToBuf};

/// Should be followed by a
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct MatrixMapData {
    height: u32,
    width: u32,
    // data is always of length `self.width * self.height`
    data: Vec<u32>,
}
impl MatrixMapData {
    /// Matrix width in cells.
    pub fn width(&self) -> u32 {
        self.width
    }
    /// Matrix height in cells.
    pub fn height(&self) -> u32 {
        self.height
    }
    /// Zone-local LED indices in row-major order; u32::MAX marks a hole.
    pub fn indices(&self) -> &[u32] {
        &self.data
    }
}
impl SerToBuf for Option<MatrixMapData> {
    fn serialize(&self, buf: &mut crate::WriteMessage) -> crate::OpenRgbResult<()> {
        match self {
            Self::Some(m) => {
                let len = (m.width * m.height) as u16;
                buf.write_value(len)?;
                if len > 0 {
                    buf.write_value(m)?;
                }
                Ok(())
            }
            // should be unreachable, treat it as matrix with dimension = 0
            Self::None => buf.write_value(0_u16),
        }
    }
}

impl DeserFromBuf for Option<MatrixMapData> {
    /// Deserializes [`zone_matrix_len` + `matrix_map_data`]
    fn deserialize(buf: &mut ReceivedMessage<'_>) -> crate::OpenRgbResult<Self> {
        let len = buf.read_value::<u16>()?;
        if len == 0 {
            return Ok(None);
        }
        if len < 8 || (len - 8) % 4 != 0 {
            return Err(crate::OpenRgbError::ProtocolError(
                "OpenRGB matrix byte length is invalid".into(),
            ));
        }
        let matrix = buf.read_value::<MatrixMapData>()?;
        if matrix.indices().len() * 4 + 8 != len as usize {
            return Err(crate::OpenRgbError::ProtocolError(
                "OpenRGB matrix byte length does not match dimensions".into(),
            ));
        }
        Ok(Some(matrix))
    }
}

impl SerToBuf for MatrixMapData {
    fn serialize(&self, buf: &mut crate::WriteMessage) -> crate::OpenRgbResult<()> {
        buf.push_value(self.height)?
            .push_value(self.width)?
            .push_value(RawSlice(&self.data))?;
        Ok(())
    }
}

impl DeserFromBuf for MatrixMapData {
    fn deserialize(buf: &mut ReceivedMessage<'_>) -> crate::OpenRgbResult<Self> {
        let height = buf.read_value()?;
        let width = buf.read_value()?;
        let count = (height as usize)
            .checked_mul(width as usize)
            .ok_or_else(|| crate::OpenRgbError::ProtocolError("OpenRGB matrix overflow".into()))?;
        let data = buf.read_n_values(count)?;

        Ok(Self {
            height,
            width,
            data,
        })
    }
}
