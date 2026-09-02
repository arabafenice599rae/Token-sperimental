# Dossier di sicurezza

## Da leggere per primo

**1. La build DEVE usare `--arch v3`.** `cargo-build-sbf` emette SBPF v0 di
default, e la feature SIMD-0500 vieta il deploy di v0, v1 e v2: l'artefatto di
default viene rifiutato dal loader su qualunque cluster aggiornato.

**2. I test DEVONO girare sullo stesso artefatto che si deploya.** È il
corollario metodologico del punto 1, e conta più del punto 1. La prima versione
di questa suite usava litesvm 0.6, che non carica SBPFv3: i test verificavano un
bytecode v0 mentre il deploy ne richiedeva uno v3. Trenta test verdi su un
binario che non è quello spedito — un difetto che **non produce mai un test
rosso** e che invaliderebbe l'intero audit a valle. È stato chiuso allineando il
crate di test su litesvm 0.16 e solana-sdk 4.0.1. Chiunque tocchi le dipendenze
del crate `integration` deve verificare che questa proprietà regga ancora:
`readelf -h target/deploy/autonomous_mm.so` deve riportare `CPU Version: 3`, e i
test devono caricare quel file.

---

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

**I lamports versati al vault sono irrecuperabili.** Il cut è cappato dallo
spread del singolo trade, e lo spread di un trade è esattamente ciò che quel
trade aggiunge sopra la curva: il tetto `safe` in `treasury_cut` non morde mai
in esercizio normale. La treasury incassa il **flusso**, mai lo **stock**.
Quindi l'eccedenza sopra `rent_floor + V(S)` non cala mai — cresce di 0 o 1
lamport per trade, per arrotondamento — e chi invia fondi al vault (per
donazione o per errore) li perde: non tornano al mittente, non migliorano il
prezzo dei venditori, non raggiungono la treasury. Misurato, non dedotto:
`lamports_sent_to_the_vault_are_locked_forever`.

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

### Livello on-chain — 43 test sull'artefatto realmente deployato (litesvm)

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

**I14 — vista e settlement rispettano gli stessi limiti.** `quote` applicava il
tetto `MAX_SUPPLY`? No: preventivava anche acquisti che `buy` rifiuta. Un
preventivo per un trade impossibile è informazione falsa, e i frontend si fidano
del preventivo — è il suo scopo. Ora `quote` restituisce `MaxSupplyExceeded`
esattamente come `buy`, e due test lo fissano: uno oltre il tetto (rifiuto con
lo stesso codice d'errore del settlement) e uno **al limite esatto**, dove il
preventivo deve invece funzionare e coincidere col costo effettivo — così il
tetto non può essere applicato con un fuori-di-uno.

Il rimborso 0 per Δ maggiore della supply resta com'era: lì il comportamento è
già onesto, perché quella vendita non è impossibile, è semplicemente vuota.

### Limiti estremi, costo e robustezza

**La curva a scala piena.** Comprare l'intera supply in una sola istruzione
calcola `N(MAX) · P0 ≈ 3,03 × 10³⁸`, l'**88% di `u128::MAX`**: è il caso
peggiore che il `const assert` dimostra a compile time, e non era mai stato
*eseguito* sul bytecode reale. Il test lo esegue, in un colpo unico e a passi,
verificando I5 a ogni gradino sia in salita sia in discesa fino a supply 0.
Passa.

**Compute unit ai quattro angoli.** Misurate su artefatto reale, non in un solo
punto comodo:

| stato | buy | sell |
| :--- | ---: | ---: |
| supply 0, Δ = 1 | 17 237 | 17 749 |
| supply 0, Δ = MAX | 19 414 | 19 748 |
| supply ≈ MAX, Δ = 1 | 19 222 | 19 918 |
| supply 5·10¹¹, Δ = 5·10¹¹ | 19 413 | **20 123** |
| `quote` a supply alta | 7 914 | — |

Massimo osservato **20 123 CU**, il 10% del budget di default. Il test fissa
una soglia di guardia a 60 000: una modifica che triplicasse il costo cade in
test prima che la scopra un utente.

**Nessun token fuori dalla curva.** `mint authority` e `freeze authority` sono
entrambe la PDA `state`, con 6 decimali; un `MintTo` firmato da un estraneo è
rifiutato, e nessuno può firmare per la PDA. Se fosse possibile mintare fuori
dal programma, si potrebbero rivendere token mai pagati svuotando il vault
**senza violare alcun invariante interno** — è la prima domanda di un audit, e
ora ha una risposta eseguibile.

**Input malformati.** Settanta payload — vuoti, troncati, discriminator
inesistenti, argomenti a metà, mille byte di spazzatura, e un lotto
pseudo-casuale — devono produrre errori puliti e **mai** un
`ProgramFailedToComplete`, che è la firma esatta del panic da `is_on_curve`
trovato in questa campagna. Nessun input malformato muove supply o vault.

### Casi al contorno

- **Aliasing**: con `user == treasury` il cut è un trasferimento verso sé
  stessi; il vault riceve e paga esattamente come in un trade normale e I5
  regge. Lo stesso token account passato due volte nella stessa transazione
  resta coerente.
- **Donazioni**: vedi il modello di fiducia — restano bloccate nel vault.
- **`initialize`**: emette `InitializeEvent` con mint e treasury corretti, e
  scrive nello stato mint, treasury e i due bump attesi; il vault parte
  esattamente al rent floor, system-owned e senza dati.
- **`quote(0)`** restituisce `ZeroAmount` come `buy(0)` e `sell(0)`: I14 vale
  ora su entrambi i limiti, quantità nulla e tetto di supply.
- **Concorrenza** (su validator reale): quattro acquisti spediti senza
  attendere conferma; l'ordine di esecuzione lo decide il runtime, ma il totale
  pagato coincide **al lamport** con la somma dei prezzi sequenziali, perché i
  gradini della curva sono gli stessi in qualunque ordine si percorrano.

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
3. **verificare che il program id deployato coincida con `declare_id!`** — un
   `buy` di prova che riesce lo dimostra: se non coincidessero, il programma
   rifiuterebbe con `DeclaredProgramIdMismatch`. Va fatto **prima** del passo
   successivo: bruciare l'authority di un programma la cui `declare_id!` non
   corrisponde all'indirizzo reale lo rende inutilizzabile per sempre, ed è il
   modo più banale e più devastante di sbagliare un deploy immutabile;
4. `solana program set-upgrade-authority <id> --final`;
5. `solana program show` → `Authority: none`;
6. nuovo `buy` e `sell` → funzionano, stessi prezzi, eventi presenti, I5 rispettato;
7. tentativo di re-deploy → «Program is no longer upgradeable».

Il punto 6 è quello che conta: finché il passo 3 non è eseguito, ogni invariante
in questo documento è una promessa revocabile dal detentore dell'authority.

### Mutation testing

La suite è stata validata iniettando **venti** difetti deliberati nel
programma e verificando che i test li rilevino. Ogni mutazione viene applicata,
l'artefatto SBF ricompilato, l'intera suite on-chain rieseguita e il sorgente
ripristinato — l'harness è in `scratchpad`, non fa parte del repo.

**Prima tornata** (matematica e percorsi principali):

| Mutazione | Prima | Dopo |
|---|---|---|
| `area_up` usa floor invece di ceil | rilevata (2 test) | — |
| `treasury_cut` senza tetto | rilevata (2 test) | — |
| `refund_sell` senza spread | **sopravvissuta** | rilevata (4 test) |
| `cost_buy` senza spread | — | rilevata (5 test) |
| rimozione del check sul deployer | rilevata (1 test) | — |
| rimozione del tetto `MAX_SUPPLY` | **sopravvissuta** | rilevata (1 test) |
| rimozione di `address = state.treasury` | rilevata (1 test) | — |
| rimozione del check I8 sul rent floor | — | rilevata (1 test) |
| rimozione del tetto `MAX_SUPPLY` da `quote` | — | rilevata (2 test) |

**Seconda tornata**, mirata ai test aggiunti per chiudere il perimetro:

| Mutazione | Esito |
|---|---|
| `quote` senza guardia `ZeroAmount` | rilevata |
| `freeze authority` al payer invece che alla PDA | rilevata |
| decimali del token 6 → 9 | rilevata |
| `initialize` non emette il suo evento | rilevata |
| vault non seminato al rent floor | rilevata |
| tetto di supply con fuori-di-uno (`<` invece di `<=`) | rilevata (3 test) |
| il cut preleva tutta l'eccedenza, non solo lo spread | rilevata |
| `ZeroAmount` in `buy` diventa un `assert!` (panic) | rilevata |
| treasury non scritta nello stato | rilevata |
| costo artificiale in `cost_buy` (regressione di CU) | rilevata, con riserva |
| `cut = spread − 1` (dentro la vecchia tolleranza) | rilevata **solo** dopo il passaggio all'uguaglianza esatta |

L'ultima riga merita una nota, perché è il caso più istruttivo. Le prime due
versioni di quella mutazione sono **sopravvissute**, e non perché il test fosse
debole: il compilatore le aveva eliminate. La prima scriveva in una variabile
poi scartata (dead store); la seconda chiamava ripetutamente `area_up` con
argomenti costanti, e LLVM ha calcolato il valore una volta sola (CSE). Solo una
catena con dipendenza dai dati, protetta da `black_box`, ha prodotto lavoro
reale — e allora il guard è scattato subito: da 20 123 a **143 205 CU**, test
rosso con il messaggio giusto.

Vale la pena notare che 143 205 CU sarebbe comunque passata sotto il budget di
default di 200 000: senza la soglia di guardia a 60 000, quella regressione
sarebbe arrivata in produzione senza che nulla la segnalasse.

## Ipotesi dell'autore falsificate dall'esecuzione

Tre convinzioni di chi ha scritto i test sono state smentite eseguendole, e
sono elencate qui perché dicono qualcosa sul grado di fiducia da accordare al
resto del documento.

1. **«Una donazione al vault finisce alla treasury al trade successivo.»**
   Falso: la treasury ha incassato 2 500 082 lamport su 2 000 000 000 donati,
   cioè solo lo spread di quel trade.
2. **«Allora vi finirà a goccia, un po' per trade.»** Falso anche questo:
   l'eccedenza non cala mai, cresce. Da qui il finding sui fondi bloccati.
3. **«Il corpus del fuzzer copre la logica del programma.»** Falso: era fatto
   di soli payload malformati, che Anchor rifiuta in deserializzazione prima di
   entrare nel corpo. Aggiungendo argomenti ben formati ma degeneri il corpus
   ha immediatamente rotto un'asserzione del test stesso, che era troppo forte.

La mutazione sul tetto di supply della prima tornata sopravviveva perché il test
negativo accettava un fallimento qualunque, e la transazione falliva davvero —
per fondi insufficienti, il motivo sbagliato. Tutti i test negativi ora
pretendono il codice d'errore atteso.

## Verifica contro le classi di vulnerabilità note

Revisione del programma contro il repertorio consolidato di errori Solana/Anchor
(le categorie Neodyme/sec3 e i vincoli Anchor). Dove possibile la proprietà è
stata **eseguita**, non dedotta.

### Checks-Effects-Interactions

Rispettato in entrambe le istruzioni, ed è visibile nell'ordine del codice.

In `sell` tutti i controlli — quantità, supply, slippage, solvibilità del vault
— precedono qualunque movimento; poi viene il **burn**, cioè l'effetto che
revoca il diritto dell'utente; solo dopo i due trasferimenti in uscita. Se
l'ordine fosse invertito, e se la reentrancy fosse possibile, un venditore
potrebbe essere pagato due volte.

In `buy` non esistono uscite di valore: l'utente paga, poi si minta. Il vault
non trasferisce nulla.

Va detto che su Solana la reentrancy classica non è disponibile — il runtime
non consente a un programma di rientrare in sé stesso via CPI — quindi qui CEI
è una cintura in più, non l'unica difesa.

### Verificato presente

| Classe | Stato |
|---|---|
| Signer mancante | `user`, `payer` e `treasury` sono `Signer` |
| Owner/tipo non verificati | tutti gli account sono tipizzati Anchor; le due `UncheckedAccount` hanno vincoli espliciti |
| PDA con bump non canonico | i bump sono calcolati all'init e **memorizzati** nello stato, poi imposti con `bump = state.…` |
| Account non legati fra loro | `address = state.mint` e `address = state.treasury` |
| Token account altrui | `token::mint = mint`, `token::authority = user` |
| CPI verso programma arbitrario | `Program<'info, System>` e `Program<'info, Token>` verificano l'id |
| Reinizializzazione | `init` su `state` e `mint`; la seconda chiamata fallisce |
| Overflow aritmetico | tutte le operazioni sono `checked_*`, **e** `overflow-checks = true` nel profilo release |
| Panic come DoS | zero `unsafe`, zero `unwrap`/`expect`/`panic!` nel corpo del programma |
| Type cosplay | discriminator Anchor su `MarketState` |
| Sysvar contraffatti | `Rent::get()` e `Clock::get()` via syscall, non account passati |
| Chiusura/realloc di account | nessuna istruzione le esegue |
| Slippage / frontrunning | guardie su entrambi i lati, verificate al lamport |
| Manipolazione di oracoli | nessun oracolo: il prezzo dipende solo dalla supply |
| Account duplicati mutabili | `user == treasury` misurato, non assunto benigno |
| **Deploy a un indirizzo diverso da `declare_id!`** | **rifiutato**: `DeclaredProgramIdMismatch` (4100), verificato eseguendo |

Nota sul token program: il programma è vincolato allo **SPL Token classico**.
È una scelta che evita per costruzione le classi di problemi introdotte dalle
estensioni Token-2022 — transfer fee e transfer hook romperebbero l'uguaglianza
fra quanto bruciato e quanto rimborsato, su cui poggia I5.

### Rilievi chiusi in questa revisione

| # | Rilievo | Esito |
|---|---|---|
| R1 | sysvar `rent` inutilizzato in `Initialize` | **rimosso** — `init` di Anchor 0.31 legge il rent via syscall; un account in meno per ogni inizializzazione |
| R2 | `mint` ricalcolava `find_program_address` a ogni istruzione | **bump memorizzato** in `MarketState`: caso peggiore da 20 123 a **18 694 CU** (−7%), `quote` da 7 914 a **6 459** (−18%) |
| R3 | nessun contatto di disclosure on-chain | **aggiunto `security_txt!`** (crate `solana-security-txt`): presente nel binario, escluso dalle build CPI |
| — | `cost - cut`: sottrazione nuda | **`checked_sub`** — vedi sotto |

La sottrazione era sicura per un teorema (`cut ≤ spread < cost`), ma un teorema
che vive nel **rapporto fra due funzioni separate** è una dipendenza invisibile
a chi ne modifica una sola. `checked_sub` lo trasforma in un errore pulito se
mai smettesse di valere. È la stessa logica dei `const assert` sui parametri
della curva: le proprietà che stanno nelle relazioni fra le parti vanno rese
esplicite nel codice, non lasciate ai commenti.

### R4 — pre-finanziamento del vault: diagnosi e decisione

Il rilievo era formulato come «eccedenza bloccata». È stato messo in
discussione con l'ipotesi opposta — che l'eccedenza sia **revenue differito**,
che defluisca alla treasury spread dopo spread — e la disputa è stata chiusa
misurando, su 200 trade dopo una donazione di 2 SOL:

| | eccedenza nel vault | incasso cumulato della treasury |
|---|---:|---:|
| dopo la donazione | 2 000 000 000 | — |
| dopo 50 trade | 2 000 000 026 | 249 453 295 |
| dopo 100 trade | 2 000 000 050 | 495 270 391 |
| dopo 200 trade | 2 000 000 100 | 1 002 584 886 |

La treasury ha incassato **oltre 1 SOL** in quel periodo, e la donazione è
rimasta intatta al lamport (più 100 di dust). Non defluisce, perché
`cut = min(spread, safe)` e **lo spread di un trade è esattamente l'eccedenza
che quel trade crea**: prelevarlo lascia lo stock preesistente dov'è. Perché
lo stock si muovesse servirebbe `cut > spread`, che L2 vieta.

**Nessun controllo aggiuntivo in `initialize`.** Un `require!(vault vuoto)`
sarebbe un vincolo che chiunque può far fallire con un transfer da 1 lamport
prima del deploy: un griefing gratuito contro la propria inizializzazione, al
prezzo di una fee. Il pre-finanziamento resta possibile e resta senza
conseguenze sul funzionamento — costa solo al mittente.

### `cut = min(spread, safe)` — decisione, con la misura che la sostiene

È stata valutata l'alternativa `cut = safe`: la treasury preleva tutta
l'eccedenza invece del solo spread, così le donazioni diventerebbero revenue
differito anziché capitale morto. I5 resterebbe identico e il vault finirebbe
esattamente a `rent_floor + V_up(S_post)`.

Applicata e misurata, **rompe il lato acquisto**. In `sell` il cut esce dal
vault, che per definizione di `safe` ha i fondi; in `buy` esce dalla **tasca
dell'utente**, e `cost − cut` va in underflow non appena `safe > cost`. Con
100 SOL di eccedenza nel vault, un acquisto da 100 506 lamport viene rifiutato
con `Overflow`:

```
eccedenza nel vault : 100 000 000 000
costo del buy       :         100 506
ESITO buy  : FALLITO -> InstructionError(0, Custom(6002))
ESITO sell : RIUSCITO
```

Peggio: chiunque potrebbe donare 1 SOL al vault e **bloccare tutti gli
acquisti** sotto quella cifra fino alla prima vendita — lo stesso griefing che
sconsiglia un `require!(vault vuoto)`, ma con un bersaglio più grande. Una
versione corretta richiederebbe `cut = min(safe, cost)` in `buy` e `cut = safe`
in `sell`: un'asimmetria fra le due istruzioni, e una nuova dipendenza
invisibile — chi togliesse quel `min` in futuro romperebbe gli acquisti.

Il beneficio era cosmetico (una riga meno brutta nel dossier); il costo è
superficie d'attacco reale. Si resta su `min(spread, safe)`.

Il beneficio *collaterale* di quella proposta è però stato adottato: il test
sulle donazioni verificava disuguaglianze (`l'eccedenza non cala, cresce al più
di 2`), e ora verifica un'**uguaglianza esatta** — l'eccedenza dopo ogni trade
è prevista al lamport dalla curva di riferimento. Vale quanto sostenuto: le
uguaglianze catturano ciò che le disuguaglianze perdonano. La mutazione
`cut = spread − 1` stava dentro la vecchia tolleranza e passava; con
l'uguaglianza cade al primo trade.

### Decisioni deliberate da non rovesciare

- **SPL Token classico, non Token-2022.** Non è prudenza generica: è una
  **precondizione di I5**. Una transfer fee o un transfer hook romperebbero
  l'uguaglianza fra quanto viene bruciato e quanto viene rimborsato, su cui
  poggia l'intera garanzia di solvibilità. Migrare a Token-2022 richiede di
  rifare la dimostrazione, non solo di cambiare il program id del token.

## Cosa NON è verificato

1. **Nessun cluster condiviso.** Il deploy è stato provato su un
   `solana-test-validator` locale, a nodo singolo e senza traffico: non copre
   fee di priorità, congestione, riorganizzazioni, né il comportamento sotto
   carico concorrente.
2. **Concorrenza solo a nodo singolo.** Gli acquisti concorrenti sono stati
   verificati su un `solana-test-validator` locale: l'ordine di esecuzione non
   altera il totale pagato. Restano fuori portata i comportamenti che
   richiedono un cluster con traffico reale — MEV, sandwich sullo slippage,
   riorganizzazioni.
3. **Variazione dei parametri di rent** — vedi la sezione dedicata qui sotto:
   l'effetto è analizzato e testato, ma un aumento reale del rent non è
   simulabile in litesvm, che non espone i parametri di rent del cluster.
4. **Costo in compute unit.** Non misurato ai bordi. `Trade` ricalcola la PDA
   del mint a ogni istruzione (`bump` non memorizzato): funziona, ma è spesa
   evitabile.
5. **Analisi formale.** Gli invarianti sono verificati per campionamento
   massiccio, non dimostrati.

## Il vault sta a margine zero: cosa succede se il rent aumenta

Sul validator reale, dopo i trade, il vault si trova **esattamente** a
`rent_floor + V(S)`. Non è un caso: il cut preleva per costruzione tutto ciò che
eccede il valore della curva, quindi il margine è zero. È il significato di
«senza cushion».

`rent_floor` non è però una costante: il programma lo legge a ogni trade con
`Rent::get()`. Se Solana alzasse il minimo rent-exempt, il vault — fermo al
*vecchio* floor più `V(S)` — si troverebbe **sotto** il nuovo floor, e l'ultimo
venditore in uscita verso supply 0 verrebbe respinto da I8
(`InsufficientReserve`).

**Nessun fondo andrebbe perso.** Il vault è un system account: i depositi sono
permissionless e non passano da alcuna istruzione del programma. Chiunque —
il venditore bloccato, la treasury, un terzo qualsiasi — può trasferirgli
lamports e sbloccare l'uscita. Il sistema è auto-riparabile al costo di una
donazione dell'ordine di **0,001 SOL**.

La proprietà non è solo asserita, è testata
(`a_vault_below_the_floor_is_unblocked_by_any_donation`): il vault viene portato
sotto la soglia, il sell viene respinto con `InsufficientReserve` senza bruciare
token né intaccare il saldo dell'utente, una donazione di 500 000 lamport lo
ripiana, e la vendita successiva riesce **allo stesso prezzo di prima**. Il test
è validato per mutazione: rimuovendo il check I8 dal programma, fallisce.

Ciò che resta non verificato è l'aumento del rent in sé: litesvm non espone i
parametri di rent del cluster, quindi il test ne riproduce l'**effetto**
(vault sotto soglia), non la causa.

## Prima del deploy in produzione

Le due identità — program id e `EXPECTED_DEPLOYER` — vivono in un unico blocco
in testa a `src/lib.rs`, sdoppiate per configurazione:

```rust
#[cfg(not(feature = "production"))]  // valori di TEST, privata pubblica
#[cfg(feature = "production")]       // valori reali, da sostituire
```

Compilando con `--features production` un `const assert` **rifiuta la build**
se una delle due è ancora il segnaposto o un valore di test. Entrambe le
modalità di errore sono verificate: segnaposto lasciato, e valori di test
copiati per sbaglio nel ramo di produzione. La dimenticanza più costosa del
progetto è resa impossibile dal compilatore, non affidata a una checklist.

```bash
cargo-build-sbf --arch v3 -- --features production
```

### Sequenza

- [ ] generare la keypair del programma sulla propria macchina e conservarla:
      `solana-keygen new -o autonomous_mm-keypair.json`;
- [ ] inserire la sua pubkey nel `declare_id!` del ramo `production` e in
      `Anchor.toml`;
- [ ] inserire in `EXPECTED_DEPLOYER` la pubkey del wallet che chiamerà
      `initialize` — deve essere una chiave di cui si possiede la privata, e va
      sostituita **prima di qualsiasi cluster condiviso, devnet inclusa**;
- [ ] compilare con `--features production`: se la build passa, le identità
      sono state sostituite davvero;
- [ ] conservare in modo sicuro la keypair della treasury: le serve firmare
      `initialize`, e dopo non è più sostituibile;
- [ ] chiamare `initialize` nella stessa transazione del deploy, o comunque
      prima di rendere pubblico il program id;
- [ ] eseguire la **build verificabile** con `solana-verify` e registrarla
      pubblicamente — va fatto prima del deploy, perché dopo la finalizzazione
      un programma immutabile senza build verificabile resta una scatola nera;
- [ ] verificare che l'hash on-chain (`solana-verify get-program-hash`)
      coincida con quello della build locale;
- [ ] decidere e documentare il destino dell'upgrade authority — finché non è
      bruciata, ogni invariante di questo documento è revocabile.

### Nota sui test

La suite di integrazione è legata alle identità di test: `integration/` firma
come il deployer derivato dal seed `[7u8; 32]` e cerca il programma al suo
program id di sviluppo. È corretto che sia così — i test verificano la
configurazione di test — ma significa che **`cargo test` non va eseguito
contro un artefatto compilato con `--features production`**: fallirebbe per
identità diverse, non per un difetto.

## Ancoraggio: a quale programma si riferisce questo documento

Un documento di sicurezza che non dice **quale** binario descrive non descrive
nulla: è la lezione del difetto SBPF, dove trenta test verdi si riferivano a un
bytecode diverso da quello deployabile. Ogni revisione di questo dossier — e la
specifica che ne discende — deve aprirsi con due valori:

```
commit    <hash git del sorgente>
artefatto <executable hash del .so compilato --arch v3 --features production>
```

### L'hash giusto non è `sha256sum`

`sha256sum` sul file `.so` **non** produce il valore confrontabile con quello
on-chain. L'hash canonico è quello di `solana-verify get-executable-hash`, che
è lo sha256 del file **privato dei byte di padding a zero finali**. Sul nostro
artefatto la differenza è di 15 byte, e i due hash non hanno nulla in comune:

```
dimensione                  260 000 byte
padding a zero finale            15 byte
sha256 del file intero      bf0ae415…   <- NON confrontabile con l'on-chain
executable hash             03a07a19…   <- questo
```

Citare quello sbagliato produrrebbe una falsa discrepanza al primo confronto
con il programma deployato — o, peggio, una verifica fatta con il metro
sbagliato che passa per caso.

Stato al momento di questa revisione, con le **identità di test**:

```
executable hash (test)  03a07a19df9bbd8a7e9ebf98898d23eedcfd30070856208f9a1b17164939cf3d
```

Attenzione: l'hash sopra **non** sarà quello deployato. Sostituendo
`EXPECTED_DEPLOYER` e il program id, il binario cambia e l'hash con esso — è
esattamente il punto. Va ricalcolato sulla build di produzione, subito prima
del deploy, e confrontato dopo con quello on-chain:

```bash
cargo-build-sbf --arch v3 -- --features production
solana-verify get-executable-hash target/deploy/autonomous_mm.so

# dopo il deploy: deve coincidere
solana-verify get-program-hash <program-id>
```

### Build verificabile da terzi

L'hash calcolato in locale prova qualcosa solo a chi lo calcola. Perché un
auditor — o chiunque — possa verificare in autonomia che il `.so` on-chain
corrisponde a questo commit, serve una build riproducibile. Lo strumento è
`solana-verify` (Solana Foundation / OtterSec), che compila dentro un'immagine
Docker che pinna toolchain e platform-tools.

**Va fatto prima del deploy**: dopo la finalizzazione non si può più cambiare
nulla, e un programma immutabile senza build verificabile resta per sempre una
scatola nera.

```bash
cargo install solana-verify

# build riproducibile — gli argomenti cargo devono essere ESATTAMENTE questi,
# perche' --features production cambia il binario
solana-verify build --library-name autonomous_mm -- --features production
solana-verify get-executable-hash target/deploy/autonomous_mm.so

# dopo il deploy: l'hash on-chain deve coincidere
solana-verify get-program-hash <program-id>

# registrazione pubblica, verificabile da chiunque
solana-verify verify-from-repo \
  --program-id <program-id> \
  --library-name autonomous_mm \
  --commit-hash <commit> \
  https://github.com/arabafenice599rae/Token-sperimental \
  -- --features production
```

`solana-verify` **è installato e funzionante** in questo ambiente (v0.5.1): i
sottocomandi che non richiedono Docker — `get-executable-hash` fra questi —
sono stati eseguiti, ed è così che è emersa la differenza fra i due hash
descritta sopra.

**La build riproducibile invece non è stata eseguita**, e la ragione è stata
accertata fino in fondo anziché supposta.

Un primo tentativo era stato archiviato con «manca il daemon Docker». Era una
conclusione affrettata: il daemon **si avvia** in questo ambiente (server
29.3.1, `overlayfs`, cgroup v1) e i pull funzionano end-to-end — un'immagine da
`mcr.microsoft.com` è stata scaricata per intero, manifest e blob. Il DNS
risolve tutto correttamente.

L'ostacolo è la **policy di egress sugli host che servono i blob**:

| host | esito |
|---|---|
| `registry-1.docker.io` | 401 — raggiungibile, serve solo il token |
| `production.cloudfront.docker.com` | **`connect_rejected`** — blob di Docker Hub |
| `ghcr.io` | 401 — raggiungibile |
| `pkg-containers.githubusercontent.com` | **Forbidden** — blob di GHCR |
| `mcr.microsoft.com` | 200, pull completo riuscito |

I registry rispondono ai manifest, ma i layer transitano da CDN separati che la
policy non consente. `solana-verify build` cerca per default
`solanafoundation/solana-verifiable-build`, che vive su Docker Hub.

**Come sbloccarlo**, in ordine di pulizia:

1. allowlistare **un solo host**, `production.cloudfront.docker.com`, nella
   policy di rete dell'ambiente: è il percorso canonico e non richiede altro;
2. in alternativa, replicare l'immagine — **pinnata per digest, non per tag**,
   altrimenti la riproducibilità decade — su un registry i cui blob siano
   raggiungibili, e passarla con `solana-verify build --base-image <immagine>`.

È una restrizione di policy, non un limite tecnico: va riportata, non aggirata.
Resta l'unico punto della checklist di deploy senza prova, e va eseguito
**prima** di toccare il cluster.

## Riproducibilità

```bash
cargo test -p autonomous-mm --lib   # 33 test matematici
cargo-build-sbf --arch v3           # artefatto SBF (v3 obbligatorio, vedi difetto 5)
cd integration && cargo test        # 43 test on-chain
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
