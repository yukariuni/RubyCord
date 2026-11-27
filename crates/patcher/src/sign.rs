//! APK Signature Scheme v3 with phoenix's key and the proof of rotation from
//! the key earlier builds used (see `keys/README.md`), so either kind of
//! install updates in place. v3 only, since v1 and v2 would need the old
//! private key, so Android 9+.

use std::borrow::Cow;

use rsa::{
   RsaPrivateKey,
   pkcs1v15::SigningKey,
   pkcs8::{
      DecodePrivateKey,
      EncodePublicKey,
   },
   signature::{
      SignatureEncoding,
      Signer,
   },
   traits::PublicKeyParts,
};
use sha2::{
   Digest,
   Sha256,
   Sha512,
};

use crate::{
   Error,
   bytes::le32,
   zip::Archive,
};

const KEY: &[u8] = include_bytes!("../keys/phoenix.pk8");
const CERTIFICATE: &[u8] = include_bytes!("../keys/phoenix.der");
/// apksigner's lineage file, a 12-byte header in front of the proof-of-rotation
/// attribute value.
const LINEAGE: &[u8] = include_bytes!("../keys/lineage");

const V3_BLOCK: u32 = 0xF053_68C0;
const PROOF_OF_ROTATION: u32 = 0x3BA0_6F8C;
const ROTATION_MIN_SDK: u32 = 28;
const MAX_SDK: u32 = 0x7FFF_FFFF;
const CHUNK: usize = 1 << 20;
const MAGIC: &[u8] = b"APK Sig Block 42";

/// apksigner's choice, SHA-512 for RSA keys over 3072 bits.
#[derive(Clone, Copy)]
enum Hash {
   Sha256,
   Sha512,
}

impl Hash {
   const fn algorithm(self) -> u32 {
      match self {
         Self::Sha256 => 0x0103,
         Self::Sha512 => 0x0104,
      }
   }

   fn digest(self, parts: &[&[u8]]) -> Vec<u8> {
      fn run<D: Digest>(parts: &[&[u8]]) -> Vec<u8> {
         let mut hasher = D::new();
         for part in parts {
            hasher.update(part);
         }
         hasher.finalize().to_vec()
      }
      match self {
         Self::Sha256 => run::<Sha256>(parts),
         Self::Sha512 => run::<Sha512>(parts),
      }
   }
}

/// Signs `archive`: the signing block goes in front of the central directory
/// and the end record points past it.
pub fn sign(archive: &mut Archive<'_>) -> Result<(), Error> {
   let key = RsaPrivateKey::from_pkcs8_der(KEY).map_err(|source| Error::Key(source.to_string()))?;
   let public = key
      .to_public_key()
      .to_public_key_der()
      .map_err(|source| Error::Key(source.to_string()))?;
   let hash = if key.size() * 8 > 3072 {
      Hash::Sha512
   } else {
      Hash::Sha256
   };
   let lineage = LINEAGE
      .get(12..)
      .ok_or_else(|| Error::Key("lineage file truncated".into()))?;

   let directory = archive.directory;
   let (entries, rest) = archive.parts.split_at(directory);
   let mut chunks = Vec::new();
   for section in [entries, &rest[..1], &rest[1..]] {
      digest_section(hash, section, &mut chunks)?;
   }
   let count = le32(chunks.len())?;
   let mut top = vec![&[0x5A][..], &count];
   top.extend(chunks.iter().map(Vec::as_slice));
   let digest = hash.digest(&top);

   let algorithm = hash.algorithm().to_le_bytes();
   let sdk = [ROTATION_MIN_SDK.to_le_bytes(), MAX_SDK.to_le_bytes()].concat();
   let attributes = prefixed(&prefixed(
      &[&PROOF_OF_ROTATION.to_le_bytes()[..], lineage].concat(),
   )?)?;
   let signed_data = [
      prefixed(&prefixed(&[&algorithm[..], &prefixed(&digest)?].concat())?)?,
      prefixed(&prefixed(CERTIFICATE)?)?,
      sdk.clone(),
      attributes,
   ]
   .concat();
   let signature = match hash {
      Hash::Sha256 => SigningKey::<Sha256>::new(key).sign(&signed_data).to_vec(),
      Hash::Sha512 => SigningKey::<Sha512>::new(key).sign(&signed_data).to_vec(),
   };
   let signers = prefixed(&prefixed(
      &[
         prefixed(&signed_data)?,
         sdk,
         prefixed(&prefixed(
            &[&algorithm[..], &prefixed(&signature)?].concat(),
         )?)?,
         prefixed(public.as_bytes())?,
      ]
      .concat(),
   )?)?;

   let pair = [
      &((4 + signers.len()) as u64).to_le_bytes()[..],
      &V3_BLOCK.to_le_bytes(),
      &signers,
   ]
   .concat();
   let size = ((pair.len() + 8 + MAGIC.len()) as u64).to_le_bytes();
   let block = [&size[..], &pair, &size, MAGIC].concat();

   let start = le32(entries.iter().map(|part| part.len()).sum::<usize>() + block.len())?;
   let end = archive.parts[directory + 1].to_mut();
   end[16..20].copy_from_slice(&start);
   archive.parts.insert(directory, Cow::Owned(block));
   Ok(())
}

/// Digests of the 1 MiB chunks of one section, which may span parts but never
/// another section.
fn digest_section(
   hash: Hash,
   parts: &[Cow<'_, [u8]>],
   out: &mut Vec<Vec<u8>>,
) -> Result<(), Error> {
   let chunk = |out: &mut Vec<Vec<u8>>, bytes: &[u8]| -> Result<(), Error> {
      out.push(hash.digest(&[&[0xA5], &le32(bytes.len())?, bytes]));
      Ok(())
   };
   let mut pending = Vec::with_capacity(CHUNK);
   for part in parts {
      let mut rest = &part[..];
      if !pending.is_empty() {
         let take = rest.len().min(CHUNK - pending.len());
         pending.extend_from_slice(&rest[..take]);
         rest = &rest[take..];
         if pending.len() < CHUNK {
            continue;
         }
         chunk(out, &pending)?;
         pending.clear();
      }
      let (whole, remainder) = rest.as_chunks::<CHUNK>();
      for piece in whole {
         chunk(out, piece)?;
      }
      pending.extend_from_slice(remainder);
   }
   if !pending.is_empty() {
      chunk(out, &pending)?;
   }
   Ok(())
}

/// `bytes` behind its u32 length.
fn prefixed(bytes: &[u8]) -> Result<Vec<u8>, Error> {
   Ok([&le32(bytes.len())?[..], bytes].concat())
}
