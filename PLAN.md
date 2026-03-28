;; CRUCIBLE 2026-03-28 17:14
;; Blueprint: BLUEPRINT.md
(ingot :id "i1" :status forged :solo t :grade 1 :skill cli :heat 1 :max 5 :smelt 0 :proof "! grep -q '&raw' src/config.rs" :work "Fix needless borrow clippy warning at src/config.rs:345 — change &raw to raw")
(ingot :id "i2" :status molten :solo nil :grade 1 :skill cli :heat 0 :max 5 :smelt 0 :proof "cargo clippy -- -D warnings 2>&1 | grep -qv 'could not compile\\|error\\['" :work "Run cargo clippy -- -D warnings and verify zero errors after fix")
(ingot :id "i3" :status ore :solo nil :grade 1 :skill cli :heat 0 :max 5 :smelt 0 :proof "cargo test --all 2>&1 | grep -q 'test result: ok'" :work "Run cargo test --all and verify all tests pass with 0 failures")
