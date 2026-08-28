use base64::{Engine as _, engine::general_purpose::STANDARD};
use prost::Message;
use tonic::metadata::MetadataMap;

const STRATO_CONTEXT_KEY: &str = "stratocontext";
const STRATO_CONTEXT_BIN_KEY: &str = "stratocontext-bin";

#[derive(Clone, PartialEq, prost::Message)]
struct StratoContext {
    #[prost(bool, tag = "11")]
    pub is_polling: bool,
}

pub fn is_polling(metadata: &MetadataMap) -> bool {
    extract_strato_context(metadata).is_some_and(|ctx| ctx.is_polling)
}

fn extract_strato_context(metadata: &MetadataMap) -> Option<StratoContext> {
    if let Some(value) = metadata.get(STRATO_CONTEXT_KEY)
        && let Ok(s) = value.to_str()
        && let Ok(bytes) = STANDARD.decode(s.trim())
        && let Ok(ctx) = StratoContext::decode(bytes.as_slice())
    {
        return Some(ctx);
    }
    if let Some(value) = metadata.get_bin(STRATO_CONTEXT_BIN_KEY)
        && let Ok(bytes) = value.to_bytes()
        && let Ok(ctx) = StratoContext::decode(bytes.as_ref())
    {
        return Some(ctx);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use tonic::metadata::MetadataValue;

    fn encode_ascii(ctx: &StratoContext) -> MetadataMap {
        let mut map = MetadataMap::new();
        let encoded = STANDARD.encode(ctx.encode_to_vec());
        map.insert(STRATO_CONTEXT_KEY, encoded.parse().unwrap());
        map
    }

    #[test]
    fn polling_true() {
        assert!(is_polling(&encode_ascii(&StratoContext {
            is_polling: true
        })));
    }

    #[test]
    fn polling_false() {
        assert!(!is_polling(&encode_ascii(&StratoContext {
            is_polling: false
        })));
    }

    #[test]
    fn binary_metadata() {
        let mut map = MetadataMap::new();
        map.insert_bin(
            STRATO_CONTEXT_BIN_KEY,
            MetadataValue::from_bytes(&StratoContext { is_polling: true }.encode_to_vec()),
        );
        assert!(is_polling(&map));
    }

    #[test]
    fn missing_context() {
        assert!(!is_polling(&MetadataMap::new()));
    }
}
