//! The shipped lagom **skills** (`SPEC.md` §12, `CONTEXT.md` "Skill" / "Caveman
//! docs"), embedded in the binary via [`include_str!`] and re-exported so every
//! face — the CLI *and* the language bindings — bundles the identical bytes from
//! one canonical source.
//!
//! A skill is a loadable markdown document that teaches a *host* orchestrator how
//! to use lagom well. They are **inert**: lagom never loads or runs them, never
//! connects an agent, and never calls a model. The CLI's `lagom emit-skills` (and
//! a binding's equivalent) writes them into a directory the consumer chooses —
//! harness-agnostic, like `lagom emit` (lagom does not manage how a harness
//! discovers skills).
//!
//! Housing the bundle here in `lagom-proxy` — which both `lagom-cli` and the
//! `lagom-py` binding already depend on — keeps a single embedded copy rather
//! than duplicating the markdown per face.

/// The `lagom-slim-docs` skill: instructs a host to use a *small* model, once at
/// authoring time, to author caveman-style Tier-1 description overrides from the
/// slim projected surface (`SPEC.md` §4.2 Tier 0, §12). lagom itself never calls
/// a model at runtime.
pub const SLIM_DOCS: &str = include_str!("../skills/lagom-slim-docs.skill.md");

/// The `lagom-dynamic-mint` skill: instructs a host to mint an ephemeral,
/// per-agent projection scoped to that agent's lifecycle and tear it down at end
/// of life (`SPEC.md` §8, §12).
pub const DYNAMIC_MINT: &str = include_str!("../skills/lagom-dynamic-mint.skill.md");

/// The shipped skills as `(filename, body)` pairs, written verbatim by
/// `lagom emit-skills` and any binding that bundles them. Filenames carry the
/// `.skill.md` suffix so a host can discover them as skills.
pub const SHIPPED_SKILLS: &[(&str, &str)] = &[
    ("lagom-slim-docs.skill.md", SLIM_DOCS),
    ("lagom-dynamic-mint.skill.md", DYNAMIC_MINT),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_skills_are_embedded_and_named() {
        assert_eq!(SHIPPED_SKILLS.len(), 2);
        for (name, body) in SHIPPED_SKILLS {
            assert!(
                name.ends_with(".skill.md"),
                "skill file carries the .skill.md suffix: {name}"
            );
            assert!(!body.trim().is_empty(), "skill {name} is non-empty");
            // The body's first heading names the skill (sans the `.skill.md` suffix).
            let stem = name.trim_end_matches(".skill.md");
            assert!(
                body.starts_with(&format!("# {stem}")),
                "skill {name} heads with `# {stem}`"
            );
        }
    }

    /// The slim-docs skill must carry its load-bearing emphases: caveman style,
    /// pinned args hidden, and lagom never calling a model at runtime.
    #[test]
    fn slim_docs_emphasises_caveman_and_no_runtime_model() {
        assert!(SLIM_DOCS.contains("caveman"));
        assert!(SLIM_DOCS.contains("Tier-1"));
        assert!(SLIM_DOCS.to_lowercase().contains("never call"));
    }

    /// The dynamic-mint skill must describe ephemeral, per-agent-lifecycle mint
    /// and teardown.
    #[test]
    fn dynamic_mint_emphasises_lifecycle_and_teardown() {
        assert!(DYNAMIC_MINT.contains("ephemeral projection"));
        assert!(DYNAMIC_MINT.to_lowercase().contains("tear it down"));
        assert!(DYNAMIC_MINT.contains("end of life"));
    }
}
