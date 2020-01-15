use std::convert::TryInto;
use std::mem::size_of;
use aesm_client::{AesmClient, QuoteInfo};
use sgx_isa::Report;
use sgx_crypto::cmac::MacTag;
use sgx_crypto::key_exchange::DHKEPublicKey;
use ra_common::msg::{Gid, Quote, RaMsg0, RaMsg1, RaMsg2, RaMsg3, RaMsg4};
use ra_common::Stream;
use crate::error::ClientRaError;
use crate::ClientRaResult;

pub struct ClientRaContext {
    pub aesm_client: AesmClient,
    pub quote_info: QuoteInfo,
}

impl ClientRaContext {
    pub fn init() -> ClientRaResult<Self>  {
        let aesm_client = AesmClient::new();
        let quote_info = aesm_client.init_quote()?;
        Ok(Self {
            aesm_client, 
            quote_info,
        })
    }

    pub fn do_attestation(mut self, mut enclave_stream: &mut impl Stream, 
                          mut sp_stream: &mut impl Stream) -> ClientRaResult<()> {
        let msg0 = self.get_extended_epid_group_id(); 
        if cfg!(feature = "verbose") {
            eprintln!("MSG0 generated");
        }

        bincode::serialize_into(&mut sp_stream, &msg0)?;
        if cfg!(feature = "verbose") {
            eprintln!("MSG0 sent");
        }

        let msg1 = self.get_msg_1(enclave_stream);
        if cfg!(feature = "verbose") {
            eprintln!("MSG1 generated");
        }

        bincode::serialize_into(&mut sp_stream, &msg1)?;
        if cfg!(feature = "verbose") {
            eprintln!("MSG1 sent");
        }

        let msg2: RaMsg2 = bincode::deserialize_from(&mut sp_stream)?;
        if cfg!(feature = "verbose") {
            eprintln!("MSG2 received");
        }

        let msg3 = self.process_msg_2(msg2, enclave_stream)?;
        if cfg!(feature = "verbose") {
            eprintln!("MSG3 generated");
        }

        bincode::serialize_into(&mut sp_stream, &msg3)?;
        if cfg!(feature = "verbose") {
            eprintln!("MSG3 sent");
        }

        let msg4: RaMsg4 = bincode::deserialize_from(&mut sp_stream)?;
        if cfg!(feature = "verbose") {
            eprintln!("MSG4 received");
        }

        bincode::serialize_into(&mut enclave_stream, &msg4).unwrap();

        if !msg4.is_enclave_trusted {
            return Err(ClientRaError::EnclaveNotTrusted);
        }
        match msg4.is_pse_manifest_trusted {
            Some(t) => if !t { return Err(ClientRaError::PseNotTrusted); },
            None => {},
        }
        Ok(())
    }

    /// ExGID = 0 means IAS will be used for remote attestation. This function only 
    /// returns 0 for now.
    pub fn get_extended_epid_group_id(&self) -> RaMsg0 {
        RaMsg0 { exgid: 0 }
    }

    pub fn get_msg_1(&mut self, 
                     enclave_stream: &mut impl Stream) -> RaMsg1 {
        let mut g_a: DHKEPublicKey = [0u8; size_of::<DHKEPublicKey>()];
        enclave_stream.read_exact(&mut g_a[..]).unwrap();
        let gid: Gid = self.quote_info.gid().try_into().unwrap();
        RaMsg1 { gid, g_a }
    }

    pub fn process_msg_2(&self, msg2: RaMsg2, 
                         mut enclave_stream: &mut impl Stream) -> ClientRaResult<RaMsg3> {
        bincode::serialize_into(&mut enclave_stream, &msg2).unwrap();

        let sig_rl = match msg2.sig_rl {
            Some(sig_rl) => sig_rl.to_owned(),
            None => Vec::with_capacity(0),
        };
        let spid = (&msg2.spid[..]).to_owned();

        // Get a Quote and send it to enclave to sign
        let quote = Self::get_quote(&self.aesm_client,
                                    spid,
                                    sig_rl,
                                    enclave_stream)?;

        // Read MAC for msg3 from enclave
        let mut mac = [0u8; size_of::<MacTag>()];
        enclave_stream.read_exact(&mut mac).unwrap();

        Ok(RaMsg3{
            mac,
            ps_sec_prop: None, 
            quote
        })
    }

    /// Get a Quote and send it to enclave to sign
    pub fn get_quote(aesm_client: &AesmClient, 
                     spid: Vec<u8>,
                     sig_rl: Vec<u8>,
                     enclave_stream: &mut impl Stream) -> ClientRaResult<Quote> {
        let quote_info = aesm_client.init_quote()?;

        // Get report for local attestation with QE from enclave
        enclave_stream.write_all(quote_info.target_info()).unwrap();
        let mut report = vec![0u8; Report::UNPADDED_SIZE];
        enclave_stream.read_exact(&mut report[..]).unwrap();

        // Get a quote and QE report from QE and send them to enclave
        let _quote = aesm_client.get_quote(
            &quote_info,
            report,
            spid,
            sig_rl)?;
        enclave_stream.write_all(_quote.quote()).unwrap();
        enclave_stream.write_all(_quote.qe_report()).unwrap();

        let mut quote = [0u8; size_of::<Quote>()];
        quote.copy_from_slice(_quote.quote());
        Ok(quote)
    }
}