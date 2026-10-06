//! Content-addressed replay evidence for retired native Agent launches.
use crate::Store;
use serde::{Deserialize, Serialize};

const MAX_BLOCK: usize = 4096;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Claim {
    pub launch: uuid::Uuid,
    pub binding_digest: [u8; 32],
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
enum Node {
    Leaf(Claim),
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
        id.as_u128() >> 128_u32.saturating_sub(u32::from(bit))
    }
}

fn right(id: uuid::Uuid, bit: u8) -> bool {
    id.as_u128() & (1_u128 << 127_u32.saturating_sub(u32::from(bit))) != 0
}

fn prefix(node: &Node) -> uuid::Uuid {
    match node {
        Node::Leaf(claim) => claim.launch,
        Node::Branch { prefix, .. } => *prefix,
    }
}

fn read(store: &dyn Store, hash: [u8; 32]) -> Result<Node, String> {
    let bytes = store.read_history(hash)?;
    if bytes.len() > MAX_BLOCK || *blake3::hash(&bytes).as_bytes() != hash {
        return Err("Agent session history block integrity failed".into());
    }
    let node: Node =
        serde_json::from_slice(&bytes).map_err(|_| "malformed Agent session history node")?;
    if serde_json::to_vec(&node).map_err(|error| error.to_string())? != bytes {
        return Err("noncanonical Agent session history node".into());
    }
    if let Node::Branch {
        bit, left, right, ..
    } = &node
        && (*bit >= 128 || left == right)
    {
        return Err("invalid Agent session history branch".into());
    }
    Ok(node)
}

fn put(store: &dyn Store, node: &Node) -> Result<[u8; 32], String> {
    let bytes = serde_json::to_vec(node).map_err(|error| error.to_string())?;
    if bytes.len() > MAX_BLOCK {
        return Err("Agent session history block capacity exceeded".into());
    }
    let expected = *blake3::hash(&bytes).as_bytes();
    let actual = store.put_history(&bytes)?;
    if actual != expected {
        return Err("Agent session history backend returned another address".into());
    }
    Ok(actual)
}

pub(crate) fn lookup(
    store: &dyn Store,
    mut root: Option<[u8; 32]>,
    launch: uuid::Uuid,
) -> Result<Option<Claim>, String> {
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
            return Err("Agent session history path is inconsistent".into());
        }
        match node {
            Node::Leaf(claim) => return Ok((claim.launch == launch).then_some(claim)),
            Node::Branch {
                prefix,
                bit,
                left,
                right: other,
            } => {
                if previous.is_some_and(|old| bit <= old) {
                    return Err("Agent session history path is not increasing".into());
                }
                if masked(launch, bit) != masked(prefix, bit) {
                    return Ok(None);
                }
                let side = right(launch, bit);
                path = Some((prefix, bit, side));
                previous = Some(bit);
                root = Some(if side { other } else { left });
            }
        }
    }
    Err("Agent session history path capacity exceeded".into())
}

pub(crate) fn insert(
    store: &dyn Store,
    root: Option<[u8; 32]>,
    claim: Claim,
) -> Result<[u8; 32], String> {
    let Some(root) = root else {
        return put(store, &Node::Leaf(claim));
    };
    if let Some(previous) = lookup(store, Some(root), claim.launch)? {
        return if previous == claim {
            Ok(root)
        } else {
            Err("Agent session launch claim changed".into())
        };
    }
    insert_at(store, root, claim, 0)
}

fn insert_at(
    store: &dyn Store,
    hash: [u8; 32],
    claim: Claim,
    depth: usize,
) -> Result<[u8; 32], String> {
    if depth > 128 {
        return Err("Agent session history insertion depth exceeded".into());
    }
    let node = read(store, hash)?;
    let old_prefix = prefix(&node);
    let difference = (old_prefix.as_u128() ^ claim.launch.as_u128()).leading_zeros();
    let branch_bit = match &node {
        Node::Leaf(_) => 128,
        Node::Branch { bit, .. } => u32::from(*bit),
    };
    if difference < branch_bit {
        let bit = u8::try_from(difference).map_err(|error| error.to_string())?;
        let new_right = right(claim.launch, bit);
        let new = put(store, &Node::Leaf(claim))?;
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
        return Err("Agent session history conflicting leaf".into());
    };
    let side = right(claim.launch, bit);
    let child = insert_at(
        store,
        if side { other } else { left },
        claim,
        depth.saturating_add(1),
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

pub(crate) fn validate_root(store: &dyn Store, root: [u8; 32]) -> Result<(), String> {
    let _ = read(store, root)?;
    Ok(())
}
