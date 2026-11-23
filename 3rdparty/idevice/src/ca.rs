// Jackson Coxson
// Inspired by pymobiledevice3

use std::str::FromStr;

use rsa::{
    RsaPrivateKey, RsaPublicKey,
    pkcs1::DecodeRsaPublicKey,
    pkcs1v15::SigningKey,
    pkcs8::{EncodePrivateKey, LineEnding, SubjectPublicKeyInfo},
};
use sha2::Sha256;
use x509_cert::{
    Certificate,
    builder::{Builder, CertificateBuilder, Profile},
    der::EncodePem,
    name::Name,
    serial_number::SerialNumber,
    time::Validity,
};

#[derive(Clone, Debug)]
pub struct CaReturn {
    pub host_cert: Vec<u8>,
    pub dev_cert: Vec<u8>,
    pub private_key: Vec<u8>,
}

pub fn make_cert(
    signing_key: &RsaPrivateKey,
    public_key: &RsaPublicKey,
    common_name: Option<&str>,
) -> Result<Certificate, Box<dyn std::error::Error>> {
    // Create subject/issuer name
    let name = match common_name {
        Some(name) => Name::from_str(&format!("CN={name}"))?,
        None => Name::default(),
    };

    // Set validity (10 years)
    let validity = Validity::from_now(std::time::Duration::from_secs(
        365 * 9 * 12 * 31 * 24 * 60 * 60, // idk like 9 years
    ))?;

    let signing_key = SigningKey::<Sha256>::new(signing_key.clone());
    let public_key = SubjectPublicKeyInfo::from_key(public_key.clone())?;

    // Build certificate
    let cert = CertificateBuilder::new(
        Profile::Root,
        SerialNumber::new(&[1])?,
        validity,
        name,
        public_key,
        &signing_key,
    )?;

    // Sign the certificate
    let tbs_cert = cert.build()?;

    Ok(tbs_cert)
}

// Equivalent to dump_cert
fn dump_cert(cert: &Certificate) -> Result<String, Box<dyn std::error::Error>> {
    let b = cert.to_pem(LineEnding::LF)?;
    Ok(b)
}

pub(crate) fn generate_certificates(
    device_public_key_pem: &[u8],
    private_key: Option<RsaPrivateKey>,
) -> Result<CaReturn, Box<dyn std::error::Error>> {
    // Load device public key
    let device_public_key =
        RsaPublicKey::from_pkcs1_pem(std::str::from_utf8(device_public_key_pem)?)?;

    // Generate or use provided private key
    let private_key = match private_key {
        Some(p) => p,
        None => {
            let mut rng = rsa::rand_core::OsRng;
            RsaPrivateKey::new(&mut rng, 2048)?
        }
    };

    // Create CA cert
    let ca_public_key = RsaPublicKey::from(&private_key);
    let ca_cert = make_cert(&private_key, &ca_public_key, None)?;

    // Create device cert
    let dev_cert = make_cert(&private_key, &device_public_key, Some("Device"))?;

    Ok(CaReturn {
        host_cert: dump_cert(&ca_cert)?.into_bytes(),
        dev_cert: dump_cert(&dev_cert)?.into_bytes(),
        private_key: private_key
            .to_pkcs8_pem(LineEnding::LF)?
            .as_bytes()
            .to_vec(),
    })
}
