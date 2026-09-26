//! Durable model admission and finalization; neither transaction performs network I/O.
use super::*;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PendingModel {
    pub digest: String,
    pub gateway_id: String,
}
pub(crate) fn pending_receipt(id: &str) -> ActionReceipt {
    ActionReceipt {
        invocation_id: id.into(),
        status: "pending".into(),
        error: None,
        output: None,
    }
}
pub(crate) fn validate_text(text: &str) -> Result<()> {
    if text.trim().is_empty()
        || serde_json::to_vec(&serde_json::json!([{"role":"user","content":text}]))?.len() > 32768
    {
        bail!("invalid model input")
    }
    Ok(())
}
impl Store {
    pub(crate) fn begin_model(
        &mut self,
        source: [u8; 32],
        request: &ActionRequest,
        digest: String,
        mesh_link: Option<&str>,
    ) -> Result<ActionReceipt> {
        self.ensure_reconciled()?;
        let scope = format!("{}:{}", hex::encode(source), request.invocation_id);
        if let Some(pending) = self.state.pending_models.get(&scope) {
            if pending.digest != digest {
                bail!("invocation substitution")
            }
        } else {
            // RFC 9562 UUIDv8: first 128 bits of domain-separated BLAKE3 over a
            // canonical JSON tuple (machine, certified device, invocation, call index).
            let bytes = serde_json::to_vec(&(
                self.state.machine_id.as_str(),
                hex::encode(source),
                request.invocation_id.as_str(),
                0u32,
            ))?;
            let hash = blake3::derive_key("tilo cloud model request uuid v8 v1", &bytes);
            let mut id: [u8; 16] = hash[..16].try_into().unwrap();
            id[6] = (id[6] & 0x0f) | 0x80;
            id[8] = (id[8] & 0x3f) | 0x80;
            let pending = PendingModel {
                digest,
                gateway_id: uuid::Uuid::from_bytes(id).to_string(),
            };
            let mut state = self.state.clone();
            if let Some(link) = mesh_link {
                state.mesh.pending_scopes.insert(scope.clone(), link.into());
            }
            state.pending_models.insert(scope, pending);
            self.commit(state)?;
        }
        Ok(pending_receipt(&request.invocation_id))
    }
    pub(crate) fn finish_model_authorized(
        &mut self,
        source: [u8; 32],
        request: &ActionRequest,
        expected: &PendingModel,
        outcome: gateway::Outcome,
        attribution: Option<serde_json::Value>,
    ) -> Result<ActionReceipt> {
        self.ensure_reconciled()?;
        let scope = format!("{}:{}", hex::encode(source), request.invocation_id);
        let digest = blake3::hash(&serde_json::to_vec(request)?)
            .to_hex()
            .to_string();
        if digest != expected.digest {
            bail!("invocation substitution")
        }
        if let Some((prior, receipt)) = self.state.receipts.get(&scope) {
            if prior != &digest {
                bail!("invocation substitution")
            }
            return Ok(receipt.clone());
        }
        let pending = self
            .state
            .pending_models
            .get(&scope)
            .ok_or_else(|| anyhow!("missing model admission"))?;
        if pending.digest != digest || pending.gateway_id != expected.gateway_id {
            bail!("model admission mismatch")
        }
        let mut receipt = pending_receipt(&request.invocation_id);
        match outcome {
            gateway::Outcome::Pending => return Ok(receipt),
            gateway::Outcome::Completed(text) => {
                receipt.status = "completed".into();
                receipt.output = Some(serde_json::json!({"text":text}));
            }
            gateway::Outcome::Failed(code) => {
                receipt.status = "failed".into();
                receipt.error = Some(code.into());
            }
        }
        if let Some(attribution) = attribution {
            if receipt.output.is_none() {
                receipt.output = Some(serde_json::json!({}));
            }
            if let (Some(output), Some(fields)) = (
                receipt.output.as_mut().and_then(|v| v.as_object_mut()),
                attribution.as_object(),
            ) {
                output.extend(fields.clone());
            }
        }
        let mut state = self.state.clone();
        state.pending_models.remove(&scope);
        state.mesh.pending_scopes.remove(&scope);
        state.receipts.insert(scope, (digest, receipt.clone()));
        self.commit(state)?;
        Ok(receipt)
    }
}
