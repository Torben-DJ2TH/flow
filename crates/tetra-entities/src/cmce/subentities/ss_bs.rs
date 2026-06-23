use crate::MessageQueue;
use tetra_core::tetra_entities::TetraEntity;
use tetra_core::{BitBuffer, Layer2Service, Sap, SsiType, TetraAddress};
use tetra_pdus::cmce::enums::cmce_pdu_type_ul::CmcePduTypeUl;
use tetra_pdus::cmce::pdus::cmce_function_not_supported::CmceFunctionNotSupported;
use tetra_saps::lcmc::LcmcMleUnitdataReq;
use tetra_saps::{SapMsg, SapMsgInner};

/// Clause 12 Supplementary Services CMCE sub-entity
pub struct SsBsSubentity {}

impl SsBsSubentity {
    pub fn new() -> Self {
        SsBsSubentity {}
    }

    pub fn route_re_deliver(&mut self, queue: &mut MessageQueue, mut message: SapMsg) {
        tracing::trace!("route_re_deliver");

        let SapMsgInner::LcmcMleUnitdataInd(prim) = &mut message.msg else {
            tracing::error!("BUG: unexpected message or state -- routing error");
            return;
        };

        // ETSI EN 300 392-2 §14.7.2.5:
        // BS does not support supplementary services (SS). Respond with
        // D-CMCE-FUNCTION-NOT-SUPPORTED, function_not_supported_pointer=0
        // (the PDU type itself is not supported, not a specific field).
        tracing::debug!(
            "CMCE: received U-FACILITY from ISSI {} — responding D-CMCE-FUNCTION-NOT-SUPPORTED",
            prim.received_tetra_address.ssi
        );

        let response = CmceFunctionNotSupported {
            not_supported_pdu_type: CmcePduTypeUl::UFacility.into_raw() as u8,
            call_identifier_present: false,
            call_identifier: None,
            function_not_supported_pointer: 0,
            length_of_received_pdu_extract: None,
            received_pdu_extract: None,
        };

        let mut sdu = BitBuffer::new_autoexpand(16);
        if let Err(e) = response.to_bitbuf(&mut sdu) {
            tracing::error!("Failed to serialize D-CMCE-FUNCTION-NOT-SUPPORTED: {:?}", e);
            return;
        }
        sdu.seek(0);

        queue.push_back(SapMsg {
            sap: Sap::LcmcSap,
            src: TetraEntity::Cmce,
            dest: TetraEntity::Mle,
            msg: SapMsgInner::LcmcMleUnitdataReq(LcmcMleUnitdataReq {
                sdu,
                handle: prim.handle,
                endpoint_id: prim.endpoint_id,
                link_id: prim.link_id,
                layer2service: Layer2Service::Unacknowledged,
                pdu_prio: 0,
                layer2_qos: 0,
                stealing_permission: false,
                stealing_repeats_flag: false,
                chan_alloc: None,
                main_address: TetraAddress::new(prim.received_tetra_address.ssi, SsiType::Issi),
                tx_reporter: None,
            }),
        });
    }
}
