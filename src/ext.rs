//! Retrieval-extension chain management (SPEC §9): init at vault
//! creation, open + lazy reindex on query (SPEC §9.6).

use std::path::Path;

use metatheca::Vault;

use crate::error::{Error, Result};

/// The generic document extractor (SPEC §9.2) — no cuj-specific
/// extractor exists.
pub const EXTRACTOR_ID: &str = "metatheca-docs:1";
pub const ANALYZER_ID: &str = "unicode-segmentation:1.12:und";
pub const MODEL_ID: &str = "feathash:1:64";
pub const METRIC: &str = "cosine";

pub fn init_chains(root: &Path) -> Result<()> {
    let xid = taxopsis::ExtractorId::parse(EXTRACTOR_ID)?;
    taxopsis::Chain::init(root, &xid, taxopsis::DEFAULT_ROUTING)?;
    logopsis::Chain::init(
        root,
        logopsis::AnalyzerId::parse(ANALYZER_ID)?,
        logopsis::DEFAULT_K1,
        logopsis::DEFAULT_B,
    )?;
    semopsis::Chain::init(
        root,
        semopsis::ModelId::parse(MODEL_ID)?,
        semopsis::Metric::parse(METRIC)?,
    )?;
    // Hybrid fusion (SPEC §zetetes binding): default RRF
    // parameters, pins advanced lazily by zetetes_fresh.
    zetetes::Chain::init(root, None, None, None, None, None)?;
    Ok(())
}

/// Recreate any chain that is missing or unopenable. `init_chains`
/// runs after the vault's first commit, so a failure there would
/// otherwise leave the vault with no route back to working
/// retrieval — `reindex` calls this before advancing.
pub fn ensure_chains(root: &Path) -> Result<()> {
    if open_taxopsis(root).is_err() {
        let xid = taxopsis::ExtractorId::parse(EXTRACTOR_ID)?;
        taxopsis::Chain::init(root, &xid, taxopsis::DEFAULT_ROUTING)?;
    }
    if open_logopsis(root).is_err() {
        logopsis::Chain::init(
            root,
            logopsis::AnalyzerId::parse(ANALYZER_ID)?,
            logopsis::DEFAULT_K1,
            logopsis::DEFAULT_B,
        )?;
    }
    if semopsis::Chain::open(root).is_err() {
        semopsis::Chain::init(
            root,
            semopsis::ModelId::parse(MODEL_ID)?,
            semopsis::Metric::parse(METRIC)?,
        )?;
    }
    if open_zetetes(root).is_err() || matches!(open_zetetes(root), Ok(None)) {
        zetetes::Chain::init(root, None, None, None, None, None)?;
    }
    Ok(())
}

pub fn open_taxopsis(root: &Path) -> Result<taxopsis::Chain> {
    Ok(taxopsis::Chain::open(root)?)
}

pub fn open_logopsis(root: &Path) -> Result<logopsis::Chain> {
    Ok(logopsis::Chain::open(root)?)
}

pub fn open_semopsis(root: &Path) -> Result<semopsis::Chain> {
    let mut chain = semopsis::Chain::open(root)?;
    let state = chain.get_search_state(&chain.current_hash()?)?;
    let embedder = semopsis::resolve_embedder(&state.model).ok_or_else(|| {
        Error::Substrate(format!(
            "no embedder available for pinned model {}:{}:{}",
            state.model.family, state.model.version, state.model.dimension
        ))
    })?;
    chain.set_embedder(embedder);
    Ok(chain)
}

/// SPEC §9.6: reindex-if-stale unless the caller passed --stale.
pub fn taxopsis_fresh(
    root: &Path,
    vault: &Vault,
    stale_ok: bool,
    noisy: bool,
) -> Result<taxopsis::Chain> {
    let chain = open_taxopsis(root)?;
    let head = vault.current_state_hash()?;
    if chain.current_state()?.metatheca != head {
        if stale_ok {
            if noisy {
                eprintln!("note: taxopsis index is stale (--stale)");
            }
        } else {
            chain.reindex(None)?;
        }
    }
    Ok(chain)
}

pub fn logopsis_fresh(
    root: &Path,
    vault: &Vault,
    stale_ok: bool,
    noisy: bool,
) -> Result<logopsis::Chain> {
    let chain = open_logopsis(root)?;
    let head = vault.current_state_hash()?;
    if chain.get_search_state(&chain.current_hash()?)?.metatheca != head {
        if stale_ok {
            if noisy {
                eprintln!("note: logopsis index is stale (--stale)");
            }
        } else {
            chain.reindex(None)?;
        }
    }
    Ok(chain)
}

pub fn semopsis_fresh(
    root: &Path,
    vault: &Vault,
    stale_ok: bool,
    noisy: bool,
) -> Result<semopsis::Chain> {
    let chain = open_semopsis(root)?;
    let head = vault.current_state_hash()?;
    if chain.get_search_state(&chain.current_hash()?)?.metatheca != head {
        if stale_ok {
            if noisy {
                eprintln!("note: semopsis index is stale (--stale)");
            }
        } else {
            chain.reindex(None)?;
        }
    }
    Ok(chain)
}

/// The hybrid chain when the vault carries one (SPEC §zetetes
/// binding): Ok(None) when absent, so callers fall back to the
/// lexical ranking.
pub fn open_zetetes(root: &Path) -> Result<Option<zetetes::Chain>> {
    match zetetes::Chain::open(root) {
        Ok(chain) => Ok(Some(chain)),
        Err(zetetes::Error::NotFound(_)) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Reindex's zetetes step (SPEC §9.6: reindex forces ALL attached
/// chains): re-pin the fusion over the feeders' current states.
/// No-op on a vault without a fusion chain (§9.5 SHOULD).
pub fn advance_zetetes(root: &Path) -> Result<()> {
    if let Some(chain) = open_zetetes(root)? {
        chain.advance(&zetetes::AdvanceInputs {
            semopsis: Some(zetetes::PinInput::Current),
            logopsis: Some(zetetes::PinInput::Current),
            ..Default::default()
        })?;
    }
    Ok(())
}

/// Freshness for the fused chain (SPEC §9.6): bring logopsis and
/// semopsis up to head, then re-pin zetetes when its recorded
/// metatheca state trails.
pub fn zetetes_fresh(
    root: &Path,
    vault: &Vault,
    stale_ok: bool,
    noisy: bool,
) -> Result<Option<zetetes::Chain>> {
    let Some(chain) = open_zetetes(root)? else {
        return Ok(None);
    };
    let head = vault.current_state_hash()?;
    if chain.get_state(&chain.current_hash()?)?.metatheca != head {
        if stale_ok {
            if noisy {
                eprintln!("note: zetetes index is stale (--stale)");
            }
        } else {
            logopsis_fresh(root, vault, false, noisy)?;
            semopsis_fresh(root, vault, false, noisy)?;
            chain.advance(&zetetes::AdvanceInputs {
                semopsis: Some(zetetes::PinInput::Current),
                logopsis: Some(zetetes::PinInput::Current),
                ..Default::default()
            })?;
        }
    }
    Ok(Some(chain))
}
