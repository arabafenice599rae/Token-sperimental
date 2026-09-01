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

Il difetto 3 merita attenzione: sotto `target_os = "solana"` il corpo di
`is_on_curve` è letteralmente `unimplemented!()`. Il programma compilava, i test
unitari passavano, e `initialize` sarebbe **panicata su ogni cluster reale** —
il mercato non sarebbe mai stato inizializzabile. Nessuna analisi statica lo
avrebbe mostrato: è servito eseguire l'artefatto SBF.

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

### Livello on-chain — 24 test su artefatto SBF reale (litesvm)

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

1. **Nessun deploy su cluster reale.** I test girano in `litesvm`, che è una VM
   in-process: fedele all'esecuzione SBF, ma non copre fee di priorità,
   congestione, limiti di compute unit sotto carico, né il comportamento del
   loader in upgrade.
2. **`quote` non è testata.** Restituisce dati via return-data Anchor; il
   percorso non è coperto da alcun test.
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
      seed `[7u8; 32]`, di cui chiunque legga questo repo possiede la privata;
- [ ] sostituire il program id con quello della keypair di deploy reale;
- [ ] decidere e documentare il destino dell'upgrade authority;
- [ ] conservare in modo sicuro la keypair della treasury: le serve firmare
      `initialize`, e dopo non è più sostituibile;
- [ ] chiamare `initialize` nella stessa transazione del deploy, o comunque
      prima di rendere pubblico il program id.

## Riproducibilità

```bash
cargo test -p autonomous-mm --lib   # 33 test matematici
cargo-build-sbf                     # artefatto SBF
cd integration && cargo test        # 24 test on-chain
```

Il crate `integration` è un workspace separato di proposito: unificare le sue
dipendenze con quelle di Anchor rompe la build SBF.
