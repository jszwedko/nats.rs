// Copyright 2020-2022 The NATS Authors
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use crate::{tls, ConnectOptions};
use std::fs::File;
use std::io::{self, BufReader, ErrorKind};
use std::path::PathBuf;
use tokio_rustls::rustls::{self, Certificate, OwnedTrustAnchor, PrivateKey};
use tokio_rustls::webpki;

/// Loads client certificates from a `.pem` file.
/// If the pem file is found, but does not contain any certificates, it will return
/// empty set of Certificates, not error.
/// Can be used to parse only client certificates from .pem file containing both client key and certs.
pub(crate) async fn load_certs(path: PathBuf) -> io::Result<Vec<Certificate>> {
    tokio::task::spawn_blocking(move || {
        let file = std::fs::File::open(path)?;
        let mut reader = BufReader::new(file);
        let certs = rustls_pemfile::certs(&mut reader)?
            .iter()
            .map(|v| Certificate(v.clone()))
            .collect();
        Ok(certs)
    })
    .await?
}

/// Loads client key from a `.pem` file.
/// Can be used to parse only client key from .pem file containing both client key and certs.
pub(crate) async fn load_key(path: PathBuf) -> io::Result<PrivateKey> {
    tokio::task::spawn_blocking(move || {
        let file = std::fs::File::open(path)?;
        let mut reader = BufReader::new(file);

        loop {
            match rustls_pemfile::read_one(&mut reader)? {
                Some(rustls_pemfile::Item::RSAKey(key))
                | Some(rustls_pemfile::Item::PKCS8Key(key))
                | Some(rustls_pemfile::Item::ECKey(key)) => return Ok(PrivateKey(key)),
                // if public key is found, don't error, just skip it and hope to find client key next.
                Some(rustls_pemfile::Item::X509Certificate(_)) | Some(_) => {}
                None => break,
            }
        }

        Err(io::Error::new(
            ErrorKind::NotFound,
            "could not find client key in the path",
        ))
    })
    .await?
}

pub(crate) async fn config_tls(options: &ConnectOptions) -> io::Result<rustls::ClientConfig> {
    let mut root_store = rustls::RootCertStore::empty();
    // adds Mozilla root certs
    root_store.add_server_trust_anchors(webpki_roots::TLS_SERVER_ROOTS.0.iter().map(|ta| {
        OwnedTrustAnchor::from_subject_spki_name_constraints(
            ta.subject,
            ta.spki,
            ta.name_constraints,
        )
    }));

    // use provided ClientConfig or built it from options.
    let tls_config = {
        if let Some(config) = &options.tls_client_config {
            Ok(config.to_owned())
        } else {
            // Include user-provided certificates.
            for cafile in &options.certificates {
                let mut pem = BufReader::new(File::open(cafile)?);
                let certs = rustls_pemfile::certs(&mut pem)?;
                let trust_anchors = certs.iter().map(|cert| {
                    let ta = webpki::TrustAnchor::try_from_cert_der(&cert[..])
                        .map_err(|err| {
                            io::Error::new(
                                ErrorKind::InvalidInput,
                                format!("could not load certs: {}", err),
                            )
                        })
                        .unwrap();
                    OwnedTrustAnchor::from_subject_spki_name_constraints(
                        ta.subject,
                        ta.spki,
                        ta.name_constraints,
                    )
                });
                root_store.add_server_trust_anchors(trust_anchors);
            }
            let builder = rustls::ClientConfig::builder()
                .with_safe_defaults()
                .with_root_certificates(root_store);
            if let Some(cert) = options.client_cert.clone() {
                if let Some(key) = options.client_key.clone() {
                    let key = tls::load_key(key).await?;
                    let cert = tls::load_certs(cert).await?;
                    builder.with_single_cert(cert, key).map_err(|_| {
                        io::Error::new(ErrorKind::Other, "could not add certificate or key")
                    })
                } else {
                    Err(io::Error::new(
                        ErrorKind::Other,
                        "found certificate, but no key",
                    ))
                }
            } else {
                // if there are no client certs provided, connect with just TLS.
                Ok(builder.with_no_client_auth())
            }
        }
    }?;
    Ok(tls_config)
}
