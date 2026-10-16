//! Immutable owner receipt facts; a signed room root authenticates bounded UUID lookup.
//! Public receipts remain in genuine signed governance history. Blocks are retained
//! before installing the new root, and are never collected from an incomplete root set.
use crate::Store;
use serde::{Deserialize, Serialize};
use tilo_owner_commit::{OwnerCommitReceipt, OwnerCommitRequest};

pub(crate) const MAX_BLOCK: usize = 80 * 1024;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Fact {
    pub(crate) invocation: uuid::Uuid,
    pub(crate) redacted: Vec<u8>,
    pub(crate) receipt: OwnerCommitReceipt,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
enum Node {
    Leaf(Fact),
    Branch {
        prefix: uuid::Uuid,
        bit: u8,
        left: [u8; 32],
        right: [u8; 32],
    },
}

fn masked(id: uuid::Uuid, bit: u8) -> u128 {
    if bit == 0 {
        0
    } else {
        128u32
            .checked_sub(u32::from(bit))
            .and_then(|shift| id.as_u128().checked_shr(shift))
            .unwrap_or(0)
    }
}
fn right(id: uuid::Uuid, bit: u8) -> bool {
    127u32
        .checked_sub(u32::from(bit))
        .and_then(|shift| 1u128.checked_shl(shift))
        .is_some_and(|mask| id.as_u128() & mask != 0)
}
fn prefix(node: &Node) -> uuid::Uuid {
    match node {
        Node::Leaf(fact) => fact.invocation,
        Node::Branch { prefix, .. } => *prefix,
    }
}
fn read(store: &dyn Store, hash: [u8; 32]) -> Result<Node, String> {
    let bytes = store.read_history(hash)?;
    if bytes.len() > MAX_BLOCK || *blake3::hash(&bytes).as_bytes() != hash {
        return Err("native effect history block integrity failed".into());
    }
    let node: Node = serde_json::from_slice(&bytes).map_err(|_| "malformed effect history node")?;
    if serde_json::to_vec(&node).map_err(|e| e.to_string())? != bytes {
        return Err("noncanonical effect history node".into());
    }
    if let Node::Branch {
        bit, left, right, ..
    } = &node
        && (*bit >= 128 || left == right)
    {
        return Err("invalid effect history branch".into());
    }
    Ok(node)
}
fn put(store: &dyn Store, node: &Node) -> Result<[u8; 32], String> {
    let bytes = serde_json::to_vec(node).map_err(|e| e.to_string())?;
    if bytes.len() > MAX_BLOCK {
        return Err("effect history block capacity".into());
    }
    let hash = store.put_history(&bytes)?;
    if hash != *blake3::hash(&bytes).as_bytes() {
        return Err("effect history backend returned a different address".into());
    }
    Ok(hash)
}

/// A mismatching authenticated prefix/leaf proves nonmembership. Missing blocks never do.
pub(crate) fn lookup(
    store: &dyn Store,
    mut root: Option<[u8; 32]>,
    id: uuid::Uuid,
) -> Result<Option<Fact>, String> {
    let mut previous = None;
    let mut path = None;
    for _ in 0..=128 {
        let Some(hash) = root else {
            return Ok(None);
        };
        let node = read(store, hash)?;
        if let Some((parent, bit, side)) = path
            && (masked(prefix(&node), bit) != masked(parent, bit)
                || right(prefix(&node), bit) != side)
        {
            return Err("effect history child contradicts its authenticated path".into());
        }
        match node {
            Node::Leaf(fact) => return Ok((fact.invocation == id).then_some(fact)),
            Node::Branch {
                prefix,
                bit,
                left,
                right: other,
            } => {
                if previous.is_some_and(|old| bit <= old) {
                    return Err("effect history path is not strictly increasing".into());
                }
                if masked(id, bit) != masked(prefix, bit) {
                    return Ok(None);
                }
                let side = right(id, bit);
                path = Some((prefix, bit, side));
                previous = Some(bit);
                root = Some(if side { other } else { left });
            }
        }
    }
    Err("effect history path capacity exceeded".into())
}

pub(crate) fn insert(
    store: &dyn Store,
    root: Option<[u8; 32]>,
    fact: Fact,
) -> Result<[u8; 32], String> {
    let Some(root) = root else {
        return put(store, &Node::Leaf(fact));
    };
    // Validate the exact existing path before creating anything. A damaged path
    // cannot be overwritten and mistaken for a previously unseen UUID.
    if let Some(previous) = lookup(store, Some(root), fact.invocation)? {
        if previous.receipt.binding != fact.receipt.binding
            || previous.receipt.generation != fact.receipt.generation
            || previous.receipt.aggregate_hash != fact.receipt.aggregate_hash
            || previous.redacted != fact.redacted
        {
            return Err("effect history UUID is immutable".into());
        }
        return Ok(root);
    }
    insert_at(store, root, fact, 0)
}
fn insert_at(
    store: &dyn Store,
    hash: [u8; 32],
    fact: Fact,
    depth: usize,
) -> Result<[u8; 32], String> {
    if depth > 128 {
        return Err("effect history insertion depth exceeded".into());
    }
    let node = read(store, hash)?;
    let old_prefix = prefix(&node);
    let difference = (old_prefix.as_u128() ^ fact.invocation.as_u128()).leading_zeros();
    let branch_bit = match &node {
        Node::Leaf(_) => 128,
        Node::Branch { bit, .. } => u32::from(*bit),
    };
    if difference < branch_bit {
        let bit = u8::try_from(difference).map_err(|e| e.to_string())?;
        let new_right = right(fact.invocation, bit);
        let new = put(store, &Node::Leaf(fact))?;
        return put(
            store,
            &Node::Branch {
                prefix: old_prefix,
                bit,
                left: if new_right { hash } else { new },
                right: if new_right { new } else { hash },
            },
        );
    }
    let Node::Branch {
        prefix,
        bit,
        left,
        right: other,
    } = node
    else {
        return Err("effect history conflicting leaf".into());
    };
    let side = right(fact.invocation, bit);
    let child = insert_at(
        store,
        if side { other } else { left },
        fact,
        depth
            .checked_add(1)
            .ok_or("effect history depth exhausted")?,
    )?;
    put(
        store,
        &Node::Branch {
            prefix,
            bit,
            left: if side { left } else { child },
            right: if side { child } else { other },
        },
    )
}

pub(crate) fn request(room: &str, fact: &Fact) -> OwnerCommitRequest {
    OwnerCommitRequest {
        room: room.into(),
        operation: tilo_owner_commit::CommitOperation::EffectReceipt {
            invocation: fact.invocation,
            redacted: fact.redacted.clone(),
        },
    }
}

pub(crate) fn validate(
    store: &dyn Store,
    root: [u8; 32],
    max_leaves: usize,
    mut check: impl FnMut(&Fact) -> Result<(), String>,
) -> Result<(), String> {
    let mut stack = vec![(root, None)];
    let mut remaining = max_leaves
        .checked_mul(2)
        .and_then(|n| n.checked_add(1))
        .ok_or("effect history validation capacity overflow")?;
    while let Some((hash, path)) = stack.pop() {
        remaining = remaining
            .checked_sub(1)
            .ok_or("effect history node capacity")?;
        let node = read(store, hash)?;
        if let Some((parent, bit, side)) = path
            && (masked(prefix(&node), bit) != masked(parent, bit)
                || right(prefix(&node), bit) != side)
        {
            return Err("effect history child contradicts its authenticated path".into());
        }
        match node {
            Node::Leaf(fact) => check(&fact)?,
            Node::Branch {
                prefix,
                bit,
                left,
                right,
            } => {
                if path.is_some_and(|(_, previous, _)| bit <= previous) {
                    return Err("effect history branch depth is not increasing".into());
                }
                stack.push((right, Some((prefix, bit, true))));
                stack.push((left, Some((prefix, bit, false))));
                if stack.len() > 129 {
                    return Err("effect history traversal capacity".into());
                }
            }
        }
    }
    Ok(())
}
