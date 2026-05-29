use anyhow::{anyhow, Result};
use chacha20poly1305::{
    aead::{Aead, KeyInit},
    ChaCha20Poly1305, Key, Nonce,
};
use hkdf::Hkdf;
use rand::rngs::OsRng;
use sha2::{Digest, Sha256};
use x25519_dalek::{EphemeralSecret, PublicKey};

// ── Key exchange ──────────────────────────────────────────────────────────────

pub struct Keypair {
    secret: Option<EphemeralSecret>,
    public: PublicKey,
}

impl Keypair {
    pub fn generate() -> Self {
        let secret = EphemeralSecret::random_from_rng(OsRng);
        let public = PublicKey::from(&secret);
        Self {
            secret: Some(secret),
            public,
        }
    }

    pub fn public_bytes(&self) -> [u8; 32] {
        *self.public.as_bytes()
    }

    /// Consume the keypair and compute the shared session keys via X25519 DH.
    pub fn into_session(mut self, their_public: &[u8]) -> Result<Session> {
        let their_pk = PublicKey::from(
            <[u8; 32]>::try_from(their_public)
                .map_err(|_| anyhow!("Public key must be 32 bytes"))?,
        );
        let secret = self.secret.take().ok_or_else(|| anyhow!("Keypair already consumed"))?;
        let shared = secret.diffie_hellman(&their_pk);
        Session::derive(shared.as_bytes())
    }
}

// ── Symmetric session (ChaCha20-Poly1305) ────────────────────────────────────

pub struct Session {
    tx: ChaCha20Poly1305,
    rx: ChaCha20Poly1305,
}

impl Session {
    fn derive(shared_secret: &[u8]) -> Result<Self> {
        let hk = Hkdf::<Sha256>::new(None, shared_secret);
        let mut tx_key = [0u8; 32];
        let mut rx_key = [0u8; 32];
        hk.expand(b"minescale-v1-host-to-join", &mut tx_key)
            .map_err(|_| anyhow!("HKDF expand failed"))?;
        hk.expand(b"minescale-v1-join-to-host", &mut rx_key)
            .map_err(|_| anyhow!("HKDF expand failed"))?;
        Ok(Self {
            tx: ChaCha20Poly1305::new(Key::from_slice(&tx_key)),
            rx: ChaCha20Poly1305::new(Key::from_slice(&rx_key)),
        })
    }

    /// Encrypt plaintext.  Returns [nonce(12) | ciphertext+tag].
    pub fn encrypt(&self, plaintext: &[u8]) -> Result<Vec<u8>> {
        let nonce_bytes: [u8; 12] = rand::random();
        let nonce = Nonce::from_slice(&nonce_bytes);
        let mut out = Vec::with_capacity(12 + plaintext.len() + 16);
        out.extend_from_slice(&nonce_bytes);
        let ct = self
            .tx
            .encrypt(nonce, plaintext)
            .map_err(|e| anyhow!("Encrypt failed: {:?}", e))?;
        out.extend_from_slice(&ct);
        Ok(out)
    }

    /// Decrypt a frame produced by `encrypt`.
    pub fn decrypt(&self, data: &[u8]) -> Result<Vec<u8>> {
        if data.len() < 12 {
            return Err(anyhow!("Ciphertext too short"));
        }
        let nonce = Nonce::from_slice(&data[..12]);
        self.rx
            .decrypt(nonce, &data[12..])
            .map_err(|e| anyhow!("Decrypt failed: {:?}", e))
    }
}

// ── TLS certificate (used by QUIC/quinn) ─────────────────────────────────────

pub fn generate_self_signed_cert() -> Result<rcgen::CertifiedKey> {
    rcgen::generate_simple_self_signed(vec!["minescale".to_string()])
        .map_err(|e| anyhow!("Certificate generation failed: {}", e))
}

/// SHA-256 fingerprint of a DER-encoded certificate.
pub fn cert_fingerprint(cert_der: &[u8]) -> Vec<u8> {
    Sha256::digest(cert_der).to_vec()
}
