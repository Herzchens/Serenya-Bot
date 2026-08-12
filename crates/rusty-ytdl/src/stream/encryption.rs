use aes::cipher::block_padding::Pkcs7;
use aes::cipher::{BlockDecryptMut, KeyIvInit};

use m3u8_rs::Key;
use reqwest::Url;

use crate::utils::make_absolute_url;
use crate::VideoError;

type Aes128CbcDec = cbc::Decryptor<aes::Aes128>;

/// HLS encryption methods
#[allow(dead_code)]
#[derive(Clone, Debug)]
pub enum Encryption {
    None,
    Aes128 { key_uri: Url, iv: [u8; 16] },
    SampleAes,
}

impl Encryption {
    /// Check m3u8_key and return encryption.
    ///
    /// If encrypted, will make a query to the designated url to fetch the key
    pub async fn new(m3u8_key: &Key, base_url: &str, seq: u64) -> Result<Self, VideoError> {
        let encryption = match &m3u8_key {
            k if k.method.to_string() == *"NONE" => Self::None,
            k if k.method.to_string() == *"AES-128" => {
                if let Some(uri) = &k.uri {
                    // Bail if keyformat exists but is not "identity"
                    if let Some(keyformat) = &k.keyformat {
                        if keyformat != "identity" {
                            return Err(VideoError::EncryptionError(format!(
                                "Invalid keyformat: {}",
                                keyformat
                            )));
                        }
                    }

                    // Fetch key
                    let uri = make_absolute_url(base_url, uri)?;

                    // Parse IV
                    let mut iv = [0_u8; 16];
                    if let Some(iv_str) = &k.iv {
                        // IV is given separately
                        let iv_str = iv_str.trim_start_matches("0x");
                        hex::decode_to_slice(iv_str, &mut iv as &mut [u8])
                            .map_err(VideoError::HexError)?;
                    } else {
                        // Compute IV from segment sequence
                        iv[(16 - std::mem::size_of_val(&seq))..]
                            .copy_from_slice(&seq.to_be_bytes());
                    }

                    Self::Aes128 { key_uri: uri, iv }
                } else {
                    // Bail if no uri is found
                    return Err(VideoError::EncryptionError(
                        "No URI found for AES-128 key".to_string(),
                    ));
                }
            }
            k if k.method.to_string() == *"SAMPLE-AES" => {
                return Err(VideoError::EncryptionError(format!(
                    "Unimplemented encryption method: {}",
                    k.method
                )))
            }
            k => {
                return Err(VideoError::EncryptionError(format!(
                    "Invalid encryption method: {}",
                    k.method
                )))
            }
        };

        Ok(encryption)
    }

    /// Decrypt the given data
    pub async fn decrypt(
        &self,
        client: &reqwest_middleware::ClientWithMiddleware,
        data: &[u8],
    ) -> Result<Vec<u8>, VideoError> {
        let r = match self {
            Self::None => Vec::from(data),
            Self::Aes128 { key_uri, iv } => {
                let body = client.get(key_uri.clone()).send().await?.bytes().await?;
                let mut key = [0_u8; 16];
                if body.len() != key.len() {
                    return Err(VideoError::EncryptionError(format!(
                        "AES-128 key must be exactly 16 bytes, got {}",
                        body.len()
                    )));
                }
                key.copy_from_slice(&body);
                Aes128CbcDec::new(&key.into(), iv.into())
                    .decrypt_padded_vec_mut::<Pkcs7>(data)
                    .map_err(|e| VideoError::DecryptionError(e.to_string()))?
            }
            Self::SampleAes => unimplemented!(),
        };

        Ok(r)
    }
}

#[cfg(test)]
mod aes128_key_tests {
    use super::Encryption;
    use crate::VideoError;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    async fn key_client(
        body: &'static [u8],
    ) -> (
        reqwest_middleware::ClientWithMiddleware,
        reqwest::Url,
        tokio::task::JoinHandle<()>,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind local key server");
        let address = listener
            .local_addr()
            .expect("read local key server address");
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept key request");
            let mut request = [0_u8; 1024];
            let _ = socket.read(&mut request).await.expect("read key request");
            let headers = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            socket
                .write_all(headers.as_bytes())
                .await
                .expect("write key response headers");
            socket
                .write_all(body)
                .await
                .expect("write key response body");
        });
        let client = reqwest_middleware::ClientBuilder::new(
            reqwest::Client::builder()
                .build()
                .expect("build test client"),
        )
        .build();
        let url =
            reqwest::Url::parse(&format!("http://{address}/key.bin")).expect("parse local key URL");
        (client, url, server)
    }

    #[tokio::test]
    async fn exact_16_byte_aes128_key_control_does_not_panic() {
        let (client, key_uri, server) = key_client(&[0_u8; 16]).await;
        let encryption = Encryption::Aes128 {
            key_uri,
            iv: [0_u8; 16],
        };
        let result = encryption.decrypt(&client, &[0_u8; 16]).await;
        server.await.expect("local key server should join");
        assert!(matches!(result, Err(VideoError::DecryptionError(_))));
    }

    #[tokio::test]
    async fn short_aes128_key_returns_encryption_error_without_panicking() {
        let (client, key_uri, server) = key_client(b"tiny").await;
        let task = tokio::spawn(async move {
            let encryption = Encryption::Aes128 {
                key_uri,
                iv: [0_u8; 16],
            };
            encryption.decrypt(&client, &[0_u8; 16]).await
        });
        let result = task
            .await
            .expect("short AES-128 key response must not panic");
        server.await.expect("local key server should join");
        assert!(matches!(result, Err(VideoError::EncryptionError(_))));
    }

    #[tokio::test]
    async fn oversized_aes128_key_is_rejected_instead_of_truncated() {
        let (client, key_uri, server) = key_client(&[0_u8; 17]).await;
        let encryption = Encryption::Aes128 {
            key_uri,
            iv: [0_u8; 16],
        };
        let result = encryption.decrypt(&client, &[0_u8; 16]).await;
        server.await.expect("local key server should join");
        assert!(matches!(result, Err(VideoError::EncryptionError(_))));
    }
}
