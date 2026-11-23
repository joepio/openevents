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

## Mijn Demodam: een portaal dat alleen events kent

`/mijnoverheid` is een tweede, kleine demo naast de lab-demo: een inwonersportaal dat geen REST-API per soort gebruikt, maar alleen de eventstroom. Het is één HTML-bestand (`crates/openevents-server/src/mijnoverheid.html`, ongeveer 250 regels JavaScript, geen build) en laat zien hoe weinig logica een portaal nodig heeft als het register de rechten afdwingt.

Zo werkt het:

1. `GET /profiles` voor de namen van de handelingen ("Taak afronden").
2. `POST /events/tickets` en daarna `GET /events?ticket=…&after=0`: één SSE-stroom met alle events die deze inwoner mag lezen, eerst de historie en dan live. De app vouwt ze tot de stand van elke resource (`resourceData`, `patch` volgens RFC 7396, `deleted`).
3. Iets doen is één `POST /events` met een handeling: een vraag stellen (`nl.vng.gesprekken.gesprek.gestart`, met de eerste bijdrage in dezelfde transactie), reageren, een gesprek sluiten, een taak afronden. De UI verandert pas als het event terugkomt op de stroom, dus wat je ziet is wat het register heeft.

Wie wat mag lezen of doen staat alleen in de profielen en geldt in het register; de app filtert niets voor de veiligheid. Wel toont hij op het overzicht alleen wat over de inwoner zelf gaat, want een leesregel kan meer laten zien (een openbare melding).

De opmaak volgt de lab-demo in eenvoudige vorm: een menu met tellers, een kruimelpad en lijsten met datum, status en pijl. Er is geen eigen huisstijl of bibliotheek nodig.

Inloggen gaat in de demo met de persona's uit de dev-modus (`OPENEVENTS_DEV_AUTH=true`); de keuze staat rechtsboven.

Vergeleken met de lab-demo (gemeten lokaal, als Jan, met MijnZaken en MijnTaken op OpenEvents en de andere API's als mock):

| | Lab-demo | Mijn Demodam |
|---|---|---|
| Verzoeken bij laden | 21 in de dev-server (React doet ze daar dubbel), dus zo'n 11 naar 9 API's | 3 (profielen, ticket, stroom), plus 1 voor de demo-persona's |
| Live bijwerken | lijsten opnieuw ophalen (4 verzoeken in een minuut met de simulator aan) | zit in de stroom: 0 extra verzoeken in die minuut |
| Iets doen | per API een eigen endpoint | altijd `POST /events` |
| Code | `App.tsx` 3.735 regels TypeScript, plus build | ongeveer 250 regels JavaScript |

De lab-demo doet veel meer (producten, agenda, plannen, een assistent), dus de regels zijn geen eerlijke vergelijking van functies. Het punt is de vorm: één stroom en één schrijfroute in plaats van een client per API.
