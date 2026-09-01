# Token-sperimental

Market maker autonomo minimale su Solana: un token il cui prezzo dipende
esclusivamente dalla supply, senza oracoli, senza owner e senza istruzioni di
prelievo.

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

## Build

```bash
cargo test --lib          # test delle proprietà (matematica pura)
cargo-build-sbf           # artefatto deployabile in target/deploy/
```

La toolchain viene installata automaticamente dal SessionStart hook in
`.claude/hooks/session-start.sh`.

## Stato

Il programma compila, produce un `.so` deployabile e supera i test delle
proprietà. Non è ancora stato eseguito su un validator: le istruzioni
`initialize` / `buy` / `sell` non hanno test di integrazione.

`initialize` non ha controlli sul chiamante: chiunque può invocarla per primo e
fissare la treasury in modo permanente. Va chiamata dal deployer nella stessa
transazione del deploy, oppure il programma va modificato per vincolare il
payer.
