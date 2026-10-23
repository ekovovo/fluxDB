use std::borrow::Cow;

use crate::{sql_read_bytes::SqlReadBytes, ColumnData};

/// CLR UDT values use PLP framing; expose the raw serialized bytes.
pub(crate) async fn decode<R>(src: &mut R) -> crate::Result<ColumnData<'static>>
where
    R: SqlReadBytes + Unpin,
{
    let data = super::plp::decode(src, 0xffff_ffff).await?.map(Cow::Owned);
    Ok(ColumnData::Binary(data))
}
