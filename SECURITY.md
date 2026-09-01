# Dossier di sicurezza

Stato della verifica del programma `autonomous-mm` al termine della campagna di
test. Pensato per essere il punto di partenza di un audit esterno: dice cosa è
stato verificato, **come**, e soprattutto cosa **non** lo è.

## Modello di fiducia

Il programma non ha owner, non ha istruzioni di aggiornamento dei parametri e
non ha alcuna istruzione di prelievo. Dopo `initialize`:

- i parametri economici sono costanti di compilazione;
- la treasury è scritta una volta e mai più modificabile;
- gli unici movimenti in uscita dal vault avvengono dentro `sell`, come
  funzione pura dello stato del trade.

Chi deploya conserva l'upgrade authority del programma: finché non viene
revocata, **può sostituire il codice**. La immutabilità economica descritta
sopra vale solo a upgrade authority bruciata. Questo è il primo punto che un
audit dovrebbe verificare in fase di deploy.

## Difetti trovati e corretti

| # | Difetto | Gravità | Come è emerso |
|---|---|---|---|
| 1 | `declare_id!` con un id non base58 (`0`, `l`) | blocca la build | compilazione |
| 2 | `guard_never_bricks_trade` asseriva un valore aritmeticamente sbagliato | test errato | esecuzione dei test |
| 3 | **`Pubkey::is_on_curve()` fa panicare il programma on-chain** | **critica** | esecuzione del binario SBF in VM |
| 4 | `initialize` invocabile da chiunque (front-running della treasury) | alta | revisione del codice |
| 5 | l'artefatto di default (SBPF v0) non è deployabile | blocca il deploy | deploy su validator reale |

Il difetto 3 merita attenzione: sotto `target_os = "solana"` il corpo di
`is_on_curve` è letteralmente `unimplemented!()`. Il programma compilava, i test
unitari passavano, e `initialize` sarebbe **panicata su ogni cluster reale** —
il mercato non sarebbe mai stato inizializzabile. Nessuna analisi statica lo
avrebbe mostrato: è servito eseguire l'artefatto SBF.

Il difetto 5 è emerso solo tentando un deploy vero: `cargo-build-sbf` produce
SBPF v0 di default, e la feature **SIMD-0500 (Disable deployment of SBPF v0, v1
and v2 programs)** è attiva su Agave 4.2. Il loader rifiuta con «Detected
sbpf_version required by the executable which are not enabled». La build va
fatta con `--arch v3`. Nessun test in VM lo avrebbe mostrato: litesvm carica il
bytecode direttamente, senza passare dal loader.

La prova che la treasury sta sulla curva ed25519 (I12) è ora ottenuta
facendole **firmare** `initialize`: una PDA non possiede chiave privata e non
può produrre quella firma. È una garanzia più forte del controllo aritmetico
originale.

## Cosa è verificato

### Livello matematico — 33 test, di cui 17 property-based (proptest)

Gli arrotondamenti sono verificati **contro la specifica**, non contro una
seconda copia della formula: per il ceil si asserisce
`a*DEN >= dn*P0` e `(a-1)*DEN < dn*P0`, così un errore nella formula non può
nascondersi dietro lo stesso errore ripetuto nel test.

- correttezza esatta di `area_up` (ceil) e `area_down` (floor);
- nessun profitto da round-trip, per ogni `(supply, delta)`;
- nessun profitto spezzando acquisti o vendite in più pezzi;
- nessun profitto su **cicli chiusi arbitrari** (sequenze casuali che tornano
  alla supply di partenza);
- entità della commissione fissata a φ = 1% ±0.1 punti, su entrambi i lati;
- solvibilità I5 dopo ogni operazione su 200 000 trade simulati, con
  identità contabile `pagato − incassato = (vault − floor) + treasury`;
- **bank run**: dopo un accumulo arbitrario, tutti riescono a uscire e il vault
  resta sopra il rent floor;
- monotonia del prezzo in supply e in quantità;
- comportamento ai bordi e su overflow (`None`, mai panic);
- `guard_treasury` esaustivo sui casi al contorno, incluso l'overflow del saldo.

### Livello on-chain — 30 test sull'artefatto realmente deployato (litesvm)

I test caricano **lo stesso file `.so` compilato `--arch v3` che viene poi
deployato sul validator**. Non è un dettaglio: la prima versione di questa
suite girava su litesvm 0.6, che non supporta SBPFv3 e obbligava a testare un
bytecode v0 diverso da quello deployabile. L'allineamento su litesvm 0.16 e
solana-sdk 4.0.1 elimina quella discrepanza — vedi la nota sulle dipendenze in
fondo.

I valori prodotti dal programma sono confrontati con una **implementazione
indipendente** della curva, riscritta dalla specifica nel crate di test.

- `initialize`: rifiuta chiunque non sia `EXPECTED_DEPLOYER`; non è ripetibile;
  rifiuta treasury off-curve e ogni account di protocollo;
- `buy` e `sell` coincidono al lamport con la curva di riferimento;
- sostituzione di vault, mint, state e treasury: tutte rifiutate;
- vendita di token non posseduti e uso del token account altrui: rifiutati;
- slippage su entrambi i lati, al lamport;
- quantità nulla, oltre `MAX_SUPPLY`, oltre la supply disponibile: rifiutati
  **con il codice d'errore specifico**;
- I11: con treasury riassegnata a un programma e riempita di dati, il mercato
  continua a funzionare e il cut resta nel vault;
- I5 verificato sullo stato reale della VM dopo ogni operazione di una sequenza
  pseudo-casuale;
- bank run on-chain: ogni detentore esce e riceve esattamente quanto promesso;
- nessuna istruzione di prelievo esiste (sette nomi plausibili, tutti rifiutati);
- due trade nella stessa transazione: il secondo vede la supply aggiornata.

### `quote` — test metamorfici

Il preventivo è confrontato **al lamport** con il settlement che ne consegue,
su una griglia di undici combinazioni (supply, taglia) che include i bordi:
supply 0, Δ=1, Δ pari all'intera supply, supply prossima al massimo.

- `quote(Δ).0` coincide con il costo effettivo del `buy(Δ)` successivo;
- `quote(Δ).1` coincide con il rimborso effettivo del `sell(Δ)` successivo;
- Δ oltre la supply ⇒ rimborso 0;
- deterministica e priva di effetti sullo stato;
- monotona in quantità e in supply.

Una divergenza documentata: **`quote` non applica il tetto `MAX_SUPPLY`**, quindi
preventiva anche acquisti che `buy` rifiuterebbe con `MaxSupplyExceeded`. Non è
sfruttabile — nessun fondo si muove — ma un frontend che si fidasse del solo
preventivo mostrerebbe un prezzo per un trade impossibile. Il comportamento è
fissato da un test, così un eventuale cambiamento non passerà in silenzio.

### Deploy su validator reale

Eseguito su `solana-test-validator` 4.2.2, con l'artefatto compilato
`--arch v3`:

| Istruzione | Compute unit | Quota del budget di default (200 000) |
|---|---|---|
| `initialize` | 26 006 | 13.0% |
| `buy` | 19 397 | 9.7% |
| `sell` | 19 752 | 9.9% |

Il margine è ampio: l'aritmetica u128 non è gratuita ma resta lontana dal
limite, e non serve richiedere budget aggiuntivo.

Verificato inoltre che costi e rimborsi coincidono al lamport con la curva di
riferimento anche fuori da litesvm, e che gli eventi `TradeEvent` arrivano via
RPC nelle righe `Program data:` con tutti i campi corretti (`is_buy`, `amount`,
`lamports`, `treasury_cut`, `supply_after`).

**Cerimonia di immutabilità, provata end-to-end:**

1. deploy → upgrade authority = deployer;
2. `initialize`, un `buy`, un `sell` → tutto regolare;
3. `solana program set-upgrade-authority <id> --final`;
4. `solana program show` → `Authority: none`;
5. nuovo `buy` e `sell` → funzionano, stessi prezzi, eventi presenti, I5 rispettato;
6. tentativo di re-deploy → «Program is no longer upgradeable».

Il punto 5 è quello che conta: finché il passo 3 non è eseguito, ogni invariante
in questo documento è una promessa revocabile dal detentore dell'authority.

### Mutation testing

La suite è stata validata iniettando difetti deliberati nel programma e
verificando che i test li rilevino. Due mutazioni sono **sopravvissute al primo
giro**, e la suite è stata rafforzata di conseguenza:

| Mutazione | Prima | Dopo |
|---|---|---|
| `area_up` usa floor invece di ceil | rilevata (2 test) | — |
| `treasury_cut` senza tetto | rilevata (2 test) | — |
| `refund_sell` senza spread | **sopravvissuta** | rilevata (4 test) |
| `cost_buy` senza spread | — | rilevata (5 test) |
| rimozione del check sul deployer | rilevata (1 test) | — |
| rimozione del tetto `MAX_SUPPLY` | **sopravvissuta** | rilevata (1 test) |
| rimozione di `address = state.treasury` | rilevata (1 test) | — |

La mutazione sul tetto di supply sopravviveva perché il test negativo accettava
un fallimento qualunque, e la transazione falliva per fondi insufficienti — il
motivo sbagliato. Tutti i test negativi ora pretendono il codice d'errore atteso.

## Cosa NON è verificato

1. **Nessun cluster condiviso.** Il deploy è stato provato su un
   `solana-test-validator` locale, a nodo singolo e senza traffico: non copre
   fee di priorità, congestione, riorganizzazioni, né il comportamento sotto
   carico concorrente.
2. **Nessun test di concorrenza reale.** Due utenti che comprano nello stesso
   slot non sono stati simulati; la correttezza in quel caso discende dalla
   serializzazione delle transazioni, non da una verifica.
3. **Variazione dei parametri di rent.** I5 usa il rent floor corrente. Un
   aumento del rent deciso dal cluster potrebbe teoricamente rendere attivo il
   check I8, oggi inerte. Non simulato.
4. **Costo in compute unit.** Non misurato ai bordi. `Trade` ricalcola la PDA
   del mint a ogni istruzione (`bump` non memorizzato): funziona, ma è spesa
   evitabile.
5. **Analisi formale.** Gli invarianti sono verificati per campionamento
   massiccio, non dimostrati.

## Prima del deploy in produzione

- [ ] sostituire `EXPECTED_DEPLOYER` — oggi è la chiave di **test** derivata dal
      seed `[7u8; 32]`, di cui chiunque legga questo repo possiede la privata.
      Va fatto **prima di qualsiasi cluster condiviso, devnet inclusa**: là
      chiunque potrebbe inizializzare il mercato al posto vostro;
- [ ] compilare con `--arch v3`: l'artefatto di default non è deployabile;
- [ ] sostituire il program id con quello della keypair di deploy reale;
- [ ] decidere e documentare il destino dell'upgrade authority;
- [ ] conservare in modo sicuro la keypair della treasury: le serve firmare
      `initialize`, e dopo non è più sostituibile;
- [ ] chiamare `initialize` nella stessa transazione del deploy, o comunque
      prima di rendere pubblico il program id.

## Riproducibilità

```bash
cargo test -p autonomous-mm --lib   # 33 test matematici
cargo-build-sbf --arch v3           # artefatto SBF (v3 obbligatorio, vedi difetto 5)
cd integration && cargo test        # 30 test on-chain
```

Prova su validator reale (richiede `solana-test-validator` attivo e il
programma deployato):

```bash
cargo run --bin ceremony -- <keypair-deployer>          # CU, eventi, I5
cargo run --bin ceremony -- <keypair-deployer> post     # dopo la finalizzazione
```

### Nota sulle dipendenze

Il crate `integration` è un workspace separato di proposito: unificare le sue
dipendenze con quelle di Anchor rompe la build SBF (feature unification su
`getrandom`).

Due vincoli sono deliberati e non vanno "aggiornati" senza verificare:

- `solana-sdk` è pinnato a **=4.0.1**. La 4.1 richiede `solana-short-vec ^3.3`
  mentre litesvm 0.16 richiede `~3.2.2`: sono incompatibili e la risoluzione
  fallisce.
- `five8_core` è dichiarato con la feature **`std`** attiva. La 0.1.x è
  `no_std` e senza quella feature non implementa `Error`, il che rompe la
  compilazione di `solana-keypair 3.1.2` con un messaggio che non nomina mai
  `five8_core`.
