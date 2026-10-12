//! Separate fixed browser slots on the reviewed staging ESP.
use super::*;
use crate::setup::staging::browser;
struct Payloads {
    package: Vec<u8>,
    receipt: Vec<u8>,
    selection: Vec<u8>,
}
impl Engine<'_> {
    fn browser_slots(
        &self,
        slots: &Placeholders,
        expected: &crate::setup::browser_transfer::Expectation,
    ) -> Result<(), Refusal> {
        let package = fat::ranges_len(&slots.browser);
        if package < expected.sealed_bytes
            || package > browser::SLOT_BYTES
            || fat::ranges_len(&slots.browser_receipt) != browser::RECEIPT_BYTES
            || fat::ranges_len(&slots.browser_selection) != browser::SELECTION_BYTES
        {
            return Err(Refusal::new(
                codes::ESP_PLACEHOLDER,
                "Installer bundle lacks bounded browser reservations",
            ));
        }
        Ok(())
    }
    pub(super) fn retain_browser(&self) -> Result<(), Refusal> {
        let Some(expected) = &self.record().prepared_browser else {
            return Ok(());
        };
        let slots = self
            .placeholders
            .as_ref()
            .ok_or_else(|| Refusal::new(codes::ESP_PLACEHOLDER, "Browser slots unavailable"))?;
        self.browser_slots(slots, expected)?;
        if let Some(artifact) = self
            .request
            .prepared_setup
            .as_ref()
            .and_then(|setup| setup.browser.as_ref())
        {
            if &artifact.expectation != expected {
                return Err(Refusal::new(
                    codes::REQUEST_INVALID,
                    "Browser custody declaration changed",
                ));
            }
            let bytes = artifact
                .read(super::super::prepared_setup::caller_uid().map_err(io_refusal)?)
                .map_err(io_refusal)?;
            self.store
                .retain_browser(&self.request.operation_id, &bytes, expected)
                .map_err(io_refusal)?;
        } else {
            self.store
                .read_browser(&self.request.operation_id, expected)
                .map_err(io_refusal)?;
        }
        Ok(())
    }
    fn browser_payloads(&self, slots: &Placeholders) -> Result<Option<Payloads>, Refusal> {
        let Some(expected) = &self.record().prepared_browser else {
            return Ok(None);
        };
        self.browser_slots(slots, expected)?;
        let mut package = self
            .store
            .read_browser(&self.request.operation_id, expected)
            .map_err(io_refusal)?;
        package.resize(
            usize::try_from(fat::ranges_len(&slots.browser)).map_err(io_refusal)?,
            b' ',
        );
        let receipt = handoff::pad_json(
            &serde_json::to_vec(expected).map_err(io_refusal)?,
            browser::RECEIPT_BYTES,
        )
        .map_err(io_refusal)?;
        let setup = self.record().prepared_setup.as_ref().ok_or_else(|| {
            Refusal::new(
                codes::REQUEST_INVALID,
                "Browser preparation has no identity",
            )
        })?;
        let selection = handoff::pad_json(
            &serde_json::to_vec(&serde_json::json!({
                "schema":1,"prepared_setup":setup,"browser":expected
            }))
            .map_err(io_refusal)?,
            browser::SELECTION_BYTES,
        )
        .map_err(io_refusal)?;
        Ok(Some(Payloads {
            package,
            receipt,
            selection,
        }))
    }
    pub(super) fn check_browser(
        &self,
        disk: &Disk,
        guid: &str,
        slots: &Placeholders,
    ) -> Result<bool, Refusal> {
        let Some(payloads) = self.browser_payloads(slots)? else {
            return Ok(true);
        };
        Ok(
            self.read_ranges(disk, guid, &slots.browser)? == payloads.package
                && self.read_ranges(disk, guid, &slots.browser_receipt)? == payloads.receipt
                && self.read_ranges(disk, guid, &slots.browser_selection)? == payloads.selection,
        )
    }
    pub(super) fn deliver_browser(&mut self, disk: &Disk, guid: &str) -> Result<(), Refusal> {
        let slots = self.placeholders(disk, guid)?;
        let Some(payloads) = self.browser_payloads(&slots)? else {
            return Ok(());
        };
        self.write_ranges(disk, guid, &slots.browser, &payloads.package)?;
        self.write_ranges(disk, guid, &slots.browser_receipt, &payloads.receipt)?;
        self.write_ranges(disk, guid, &slots.browser_selection, &payloads.selection)?;
        self.platform.sync_disk(disk).map_err(io_refusal)?;
        if !self.check_browser(disk, guid, &slots)? {
            return Err(Refusal::new(
                codes::IO_FAILED,
                "Browser ciphertext failed target readback",
            ));
        }
        Ok(())
    }
}
