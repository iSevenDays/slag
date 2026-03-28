;; CRUCIBLE 2026-03-28 17:14
;; Blueprint: BLUEPRINT.md
(ingot :id "i1" :status forged :solo t :grade 1 :skill cli :heat 1 :max 5 :smelt 0 :proof "! grep -q '&raw' src/config.rs" :work "Fix needless borrow clippy warning at src/config.rs:345 — change &raw to raw")
(ingot :id "i2" :status forged :solo nil :grade 1 :skill cli :heat 1 :max 5 :smelt 0 :proof "cargo clippy -- -D warnings 2>&1 | grep -qv 'could not compile\\|error\\['" :work "Run cargo clippy -- -D warnings and verify zero errors after fix")
(ingot :id "i3" :status forged :solo nil :grade 1 :skill cli :heat 1 :max 5 :smelt 0 :proof "cargo test --all 2>&1 | grep -q 'test result: ok'" :work "Run cargo test --all and verify all tests pass with 0 failures")
(ingot :id "v_auto_c1" :status molten :solo t :grade 2 :skill cli :heat 0 :max 5 :smelt 1 :proof "cargo clippy -- -D warnings && cargo test --all" :work "Verify zero clippy warnings and all tests pass after needless borrow fix in src/config.rs. Use exit codes directly — RTK proxy reformats output so grep patterns fail.")
