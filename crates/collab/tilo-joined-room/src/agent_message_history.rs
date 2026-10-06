//! Append-only terminal UUID facts in content-addressed owner storage.
//! Lookup reads at most 129 bounded nodes. The number of retained facts is
//! unbounded by the hot queue; no replay tombstone is evicted or forgotten.
use crate::{Store, agent_message::TerminalMessage};
use serde::{Deserialize, Serialize};

const MAX_BLOCK: usize = 4096;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
enum Node {
    Leaf(TerminalMessage),
    Branch {
        prefix: uuid::Uuid,
        bit: u8,
        left: [u8; 32],
        right: [u8; 32],
    },
}

fn prefix(node: &Node) -> uuid::Uuid {
    match node {
        Node::Leaf(fact) => fact.invocation,
        Node::Branch { prefix, .. } => *prefix,
    }
}

fn masked(id: uuid::Uuid, bit: u8) -> u128 {
    128u32
        .checked_sub(u32::from(bit))
        .and_then(|shift| id.as_u128().checked_shr(shift))
        .unwrap_or(0)
}

fn right(id: uuid::Uuid, bit: u8) -> bool {
    127u32
        .checked_sub(u32::from(bit))
        .and_then(|shift| 1u128.checked_shl(shift))
        .is_some_and(|mask| id.as_u128() & mask != 0)
}

fn read(store: &dyn Store, hash: [u8; 32]) -> Result<Node, String> {
    let bytes = store.read_history(hash)?;
    if bytes.len() > MAX_BLOCK || *blake3::hash(&bytes).as_bytes() != hash {
        return Err("Agent Message history integrity failed".into());
    }
    let node: Node =
        serde_json::from_slice(&bytes).map_err(|_| "malformed Agent Message history")?;
    if serde_json::to_vec(&node).map_err(|_| "Agent Message history encoding failed")? != bytes {
        return Err("noncanonical Agent Message history".into());
    }
    match &node {
        Node::Branch {
            bit, left, right, ..
        } if *bit >= 128 || left == right => {
            return Err("invalid Agent Message history branch".into());
        }
        Node::Leaf(fact)
            if fact.invocation.is_nil()
                || fact.source_digest == [0; 32]
                || fact.generation == 0 =>
        {
            return Err("invalid terminal Agent Message history fact".into());
        }
        _ => {}
    }
    Ok(node)
}

fn put(store: &dyn Store, node: &Node) -> Result<[u8; 32], String> {
    let bytes = serde_json::to_vec(node).map_err(|_| "Agent Message history encoding failed")?;
    if bytes.len() > MAX_BLOCK {
        return Err("Agent Message history block capacity exceeded".into());
    }
    let hash = store.put_history(&bytes)?;
    if hash != *blake3::hash(&bytes).as_bytes() {
        return Err("Agent Message history backend returned another address".into());
    }
    Ok(hash)
}

pub(crate) fn lookup(
    store: &dyn Store,
    mut root: Option<[u8; 32]>,
    id: uuid::Uuid,
) -> Result<Option<TerminalMessage>, String> {
    let mut previous = None;
    let mut path = None;
    for _ in 0..=128 {
        let Some(hash) = root else { return Ok(None) };
        let node = read(store, hash)?;
        if let Some((parent, bit, side)) = path
            && (masked(prefix(&node), bit) != masked(parent, bit)
                || right(prefix(&node), bit) != side)
        {
            return Err("Agent Message history contradicts its authenticated path".into());
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
                    return Err("Agent Message history path is not increasing".into());
                }
                if masked(id, bit) != masked(prefix, bit) {
                    return Ok(None);
                }
                let side = right(id, bit);
                previous = Some(bit);
                path = Some((prefix, bit, side));
                root = Some(if side { other } else { left });
            }
        }
    }
    Err("Agent Message history path capacity exceeded".into())
}

pub(crate) fn insert(
    store: &dyn Store,
    root: Option<[u8; 32]>,
    fact: TerminalMessage,
) -> Result<[u8; 32], String> {
    if let Some(old) = lookup(store, root, fact.invocation)? {
        return if old == fact {
            root.ok_or_else(|| "Agent Message history root is absent".into())
        } else {
            Err("Agent Message terminal UUID was already consumed".into())
        };
    }
    match root {
        None => put(store, &Node::Leaf(fact)),
        Some(hash) => insert_at(store, hash, fact, 0),
    }
}

fn insert_at(
    store: &dyn Store,
    hash: [u8; 32],
    fact: TerminalMessage,
    depth: usize,
) -> Result<[u8; 32], String> {
    if depth > 128 {
        return Err("Agent Message history insertion depth exceeded".into());
    }
    let node = read(store, hash)?;
    let old_prefix = prefix(&node);
    let difference = (old_prefix.as_u128() ^ fact.invocation.as_u128()).leading_zeros();
    let branch_bit = match &node {
        Node::Leaf(_) => 128,
        Node::Branch { bit, .. } => u32::from(*bit),
    };
    if difference < branch_bit {
        let bit = u8::try_from(difference).map_err(|_| "invalid Agent Message history bit")?;
        let side = right(fact.invocation, bit);
        let new = put(store, &Node::Leaf(fact))?;
        return put(
            store,
            &Node::Branch {
                prefix: old_prefix,
                bit,
                left: if side { hash } else { new },
                right: if side { new } else { hash },
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
        return Err("Agent Message history leaf conflicts with new input".into());
    };
    let side = right(fact.invocation, bit);
    let child = insert_at(
        store,
        if side { other } else { left },
        fact,
        depth
            .checked_add(1)
            .ok_or("Agent Message history depth exhausted")?,
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
