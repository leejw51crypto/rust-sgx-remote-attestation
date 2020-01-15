use std::io::Write;
use std::mem::size_of;
use sgx_isa::{Targetinfo, Report};
use sgx_crypto::random::RandomState;
use sgx_crypto::key_exchange::OneWayAuthenticatedDHKE;
use sgx_crypto::signature::VerificationKey;
use sgx_crypto::cmac::{Cmac, MacTag};
use sgx_crypto::digest::sha256;
use ra_common::{Stream, derive_secret_keys};
use ra_common::msg::{Quote, RaMsg2, RaMsg3, RaMsg4};
use crate::error::EnclaveRaError;
use crate::EnclaveRaResult;
use crate::local_attestation;

pub struct EnclaveRaContext {
    pub key_exchange: Option<OneWayAuthenticatedDHKE>,
    pub sp_vkey: VerificationKey,
}

impl EnclaveRaContext {
    pub fn init(sp_vkey_pem: &str) -> EnclaveRaResult<Self>  {
        let rng = RandomState::new();
        let key_exchange = OneWayAuthenticatedDHKE::generate_keypair(&rng)?;
        Ok(Self {
            sp_vkey: VerificationKey::new_from_pem(sp_vkey_pem)?,
            key_exchange: Some(key_exchange),
        })
    }

    pub fn do_attestation(mut self, mut client_stream: &mut impl Stream) 
        -> EnclaveRaResult<(MacTag, MacTag)> {
            let (sk, mk) = self.process_msg_2(client_stream).unwrap();
            let msg4: RaMsg4 = bincode::deserialize_from(&mut client_stream).unwrap();
            if !msg4.is_enclave_trusted {
                return Err(EnclaveRaError::EnclaveNotTrusted);
            }
            match msg4.is_pse_manifest_trusted {
                Some(t) => if !t {
                    return Err(EnclaveRaError::PseNotTrusted);
                },
                None => {},
            }
            Ok((sk, mk))
        }

    // Return (signing key, master key)
    pub fn process_msg_2(&mut self, 
                         mut client_stream: &mut impl Stream) 
        -> EnclaveRaResult<(MacTag, MacTag)> {
            let g_a = self.key_exchange.as_ref().unwrap().get_public_key().to_owned();
            client_stream.write_all(&g_a[..]).unwrap();

            let msg2: RaMsg2 = bincode::deserialize_from(&mut client_stream).unwrap();

            // Verify and derive KDK and then other secret keys 
            let kdk = self.key_exchange.take().unwrap()
                .verify_and_derive(&msg2.g_b,
                                   &msg2.sign_gb_ga,
                                   &self.sp_vkey)
                .unwrap();
            let kdk_cmac = Cmac::new(&kdk);
            let (smk, sk, mk, vk) = derive_secret_keys(&kdk_cmac);
            let smk = Cmac::new(&smk);

            // Verify MAC tag of MSG2
            msg2.verify_mac(&smk).map_err(|_| EnclaveRaError::IntegrityError)?;

            // Obtain SHA-256(g_a || g_b || vk) 
            let mut verification_msg = Vec::new();
            verification_msg.write_all(g_a.as_ref()).unwrap();
            verification_msg.write_all(&msg2.g_b).unwrap();
            verification_msg.write_all(&vk).unwrap();
            let verification_digest = sha256(&verification_msg[..]);

            // Obtain Quote
            let quote = Self::get_quote(&verification_digest[..], client_stream)?;

            // Send MAC for msg3 to client
            let msg3 = RaMsg3::new(&smk, 
                                   None, 
                                   quote);
            client_stream.write_all(&msg3.mac).unwrap();

            Ok((sk, mk))
        }

    /// Get quote from Quote Enclave. The length of report_data must be <= 64 bytes.
    pub fn get_quote(report_data: &[u8],
                     client_stream: &mut impl Stream) -> EnclaveRaResult<Quote> {
        if report_data.len() > 64 {
            return Err(EnclaveRaError::ReportDataLongerThan64Bytes);
        }

        // Obtain QE's target info to build a report for local attestation. 
        // Then, send the report back to client.
        let mut _report_data = [0u8; 64];
        (&mut _report_data[..(report_data.len())]).copy_from_slice(report_data);
        let mut target_info = [0u8; Targetinfo::UNPADDED_SIZE];
        client_stream.read_exact(&mut target_info).unwrap();
        let target_info = Targetinfo::try_copy_from(&target_info).unwrap();
        let report = Report::for_target(&target_info, &_report_data);
        client_stream.write_all(report.as_ref()).unwrap();

        // Obtain quote and QE report from client 
        let mut quote = [0u8; size_of::<Quote>()];
        client_stream.read_exact(&mut quote[..]).unwrap();
        let qe_report_len = 432usize;
        let mut qe_report = vec![0u8; qe_report_len];
        client_stream.read_exact(&mut qe_report[..]).unwrap();

        // Verify that the report is generated by QE
        local_attestation::verify_local_attest(&qe_report[..])
            .map_err(|e| EnclaveRaError::LocalAttestation(e))?;
        Ok(quote)
    }
}