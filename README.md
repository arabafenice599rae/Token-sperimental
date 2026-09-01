# Token-sperimental

Market maker autonomo minimale su Solana: un token il cui prezzo dipende
esclusivamente dalla supply, senza oracoli, senza owner e senza istruzioni di
prelievo.

> **Due vincoli non negoziabili.** La build va fatta con `--arch v3`
> (SIMD-0500 vieta il deploy di SBPF v0/v1/v2), e i test devono girare sullo
> stesso `.so` che si deploya. Il perché è in cima a `SECURITY.md`.

## Curva

```
P(s) = P0 * (1 + s/S0)^2
V(s) = P0 * N(s) / (3*S0^2),   N(s) = 3*S0^2*s + 3*S0*s^2 + s^3
```

| Parametro | Valore |
|---|---|
| Prezzo a supply 0 | 0.1 SOL per token intero |
| Prezzo a supply massima | 1.0 SOL per token intero |
| Moltiplicatore `M` | 10× |
| Supply massima | 10^6 token (10^12 unità, 6 decimali) |
| Spread `φ` | 1.00% (0.5% per lato) |
| Raccolta totale a supply piena | ~472 076 SOL |

`BUY` paga `ceil(ΔV * (1 + φ/2))` e minta; `SELL` riceve `floor(ΔV * (1 − φ/2))`
e brucia. Lo spread incassato va alla treasury, ma solo per la parte che eccede
il valore della curva: il vault contiene sempre almeno `V(S)`, che è la garanzia
di liquidità per ogni venditore.

Gli invarianti `I1`–`I12` e i lemmi `L1`–`L3` sono documentati in testa a
`programs/autonomous-mm/src/lib.rs`.

## Interfaccia

| Istruzione | Firme richieste | Note |
|---|---|---|
| `initialize` | deployer + **treasury** | la treasury firma per provare di stare sulla curva ed25519 |
| `buy(amount, max_cost)` | utente | minta, con guardia di slippage |
| `sell(amount, min_refund)` | utente | brucia, con guardia di slippage |
| `quote(amount)` | nessuna | sola lettura; rispetta gli stessi limiti del settlement (I14) |

## Build e test

```bash
cargo test -p autonomous-mm --lib   # 33 test matematici (17 property-based)
cargo-build-sbf --arch v3           # artefatto deployabile in target/deploy/
cd integration && cargo test        # 32 test on-chain su litesvm
```

`--arch v3` non è opzionale: `cargo-build-sbf` produce SBPF v0 di default, e
SIMD-0500 ne vieta il deploy su Agave 4.2. Senza quel flag l'artefatto non è
deployabile su nessun cluster aggiornato.

`integration/` è un workspace separato di proposito: unificare le sue
dipendenze con quelle di Anchor rompe la build SBF.

### Prova su validator reale

```bash
solana-test-validator --reset &
solana program deploy target/deploy/autonomous_mm.so \
  --program-id target/deploy/autonomous_mm-keypair.json -k <deployer>
cargo run --bin ceremony -- <deployer>        # initialize, buy, sell, CU, eventi
solana program set-upgrade-authority <id> --final -k <deployer>
cargo run --bin ceremony -- <deployer> post   # il mercato regge da immutabile
```

La toolchain viene installata automaticamente dal SessionStart hook in
`.claude/hooks/session-start.sh`.

## Stato

65 test verdi: 33 sulla matematica pura, 32 sull'artefatto SBF eseguito in VM.
Il programma è stato inoltre deployato su un `solana-test-validator` reale,
dove consuma **19 397 CU** per un buy e **19 752** per un sell (budget di
default 200 000), emette gli eventi correttamente via RPC e continua a
funzionare dopo che l'upgrade authority è stata bruciata.

La suite è stata validata con mutation testing — vedi `SECURITY.md` per il
dettaglio, i difetti trovati e cosa resta non verificato.

**Non è pronto per la produzione così com'è.** `EXPECTED_DEPLOYER` contiene una
chiave di test la cui privata è derivabile dal seed `[7u8; 32]` scritto in
`integration/src/lib.rs`, e il program id appartiene a una keypair generata in
sviluppo. La checklist di deploy è in fondo a `SECURITY.md`.
