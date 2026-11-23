# MijnOverheid-demo op OpenEvents

De MijnOverheid-demo in het VNG API Lab (`mijnoverheid/`) praat met de lab-contracten MijnZaken en MijnTaken (versie `next`). OpenEvents biedt die twee contracten aan als leesweergave op wat er al in het register staat. Zo draait de demo, assistent incluis, op echte events in plaats van op mocks, en ververst hij live.

## Endpoints

| Contract | Endpoint in OpenEvents |
|---|---|
| MijnZaken `GET /zaken`, `GET /zaken/{uuid}` | `GET /mijn/zaken/zaken`, `GET /mijn/zaken/zaken/{uuid}` |
| MijnTaken `GET /taken`, `GET /taken/{uuid}` | `GET /mijn/taken/taken`, `GET /mijn/taken/taken/{uuid}` |

De basis-URL's voor een portaal zijn dus `{openevents}/mijn/zaken` en `{openevents}/mijn/taken`.

## Wie, en wat

De burger komt uit het token, net als in het contract. Eerst gelden de leesregels van het register. Daarna toont MijnZaken alleen waar de burger betrokkene is, want een leesregel kan een inwoner meer laten zien (een openbare melding, een bekendgemaakte aanvraag):

| Bron in het register | Betrokkene als | Zaak in MijnZaken |
|---|---|---|
| `nl.vng.vergunningen.aanvraag` | `aanvrager.partij` of `gemachtigde` is de burger | naam uit `omschrijving`, gesloten na besluit of intrekking |
| `nl.vng.meldingen.melding` | `melder.partij` is de burger | "Melding {subcategorie}", gesloten na afhandeling |
| `nl.vng.zgw.zaak` (via de Notificaties-adapter) | `initiatorBsn` is de claim `bsn` | identificatie, omschrijving, statussen uit `nl.vng.zgw.status` |
| `nl.vng.taken.taak` (MijnTaken) | `voor.partij` is de burger | `context.urn` is de URN van de zaak |

- `uuid`: bij `urn:uuid:…` de uuid zelf, anders een vaste UUIDv5 van de id.
- `urn`: `urn:nl:gemeenten:zaak:{zaaknummer}`. Taken verwijzen ernaar met `context.urn`, dus `GET /taken?context={urn}` werkt.
- `statushistorie`: uit de eigen events van de zaak. Elke nieuwe `status` is een stap, de vrije teksten uit de commits (aanvulling, verlenging, besluit) zijn de toelichting. De volgende verwachte stap staat erbij als `gepland`.
- `documenten`: de bijlagen van de aanvraag en de aanvulling, plus het besluit. Bestandsgrootte is nog `0`, want het register kent alleen de namen.
- `contactmomenten`: de gesprekken over de zaak (kanaal `vraag`).
- Een zaak van iemand anders geeft `404`, net als een zaak die niet bestaat, zodat je niet kunt raden wat er is. Het contract noemt hier `403`.

## Demo draaien

1. Start OpenEvents in dev-modus op poort 8081, met de voorbeeldprofielen, `examples/seed.sh` en `openevents-sim` (zie de README).
2. Start de demo uit het lab (`cd mijnoverheid && pnpm dev`).
3. Open de API-inspector, klik op het tandwiel en dan op **OpenEvents (lokaal)**, en op **Toepassen**. Dat zet MijnZaken en MijnTaken op `http://localhost:8081/mijn/…` en het token op `dev:https://id.example.nl/jan|rol=inwoner`. Een ander token (bijvoorbeeld `…/priya`) laat een andere inwoner zien.

Met een token en een OpenEvents-endpoint volgt de demo de eventstroom (`POST /events/tickets`, dan `GET /events?ticket=…`) en laadt hij zaken, taken en de open zaak opnieuw zodra er iets verandert.
