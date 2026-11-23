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

`/mijnoverheid` is een tweede, kleine demo naast de lab-demo: een inwonersportaal dat geen REST-API per soort gebruikt, maar alleen de eventstroom. Het is één HTML-bestand (`crates/openevents-server/src/mijnoverheid.html`, ongeveer 340 regels JavaScript, geen build) en laat zien hoe weinig logica een portaal nodig heeft als het register de rechten afdwingt.

Zo werkt het:

1. `GET /me` (wie ben ik volgens het token) en `GET /profiles` (de namen van de handelingen, zoals "Taak afronden").
2. `POST /query` met `{"mine": true}`: alles wat van deze inwoner is, over alle soorten, volgens de `mine`-regels in de profielen. Het antwoord geeft ook een `feedCursor`.
3. `POST /events/tickets` en daarna `GET /events?ticket=…&after={feedCursor}`: één SSE-stroom vanaf dat punt, live. Elk event past de stand aan (`resourceData`, `patch` volgens RFC 7396, `deleted`). Een event met `"mine": true` voor een onbekende resource is iets nieuws van de inwoner, bijvoorbeeld een taak die de gemeente klaarzet.
4. De geschiedenis van een zaak, taak of gesprek komt pas als je hem opent: `GET /subjects/{id}/events`.
5. Iets doen is één `POST /events` met een handeling: een vraag stellen (`nl.vng.gesprekken.gesprek.gestart`, met de eerste bijdrage in dezelfde transactie), reageren, een gesprek sluiten, een taak afronden. De UI verandert pas als het event terugkomt op de stroom, dus wat je ziet is wat het register heeft.

Wat van de inwoner is, wat hij mag lezen en wat hij mag doen staat alleen in de profielen en geldt in het register. De app filtert niets en kent geen velden als `melder` of `voor`.

De opmaak volgt de lab-demo in eenvoudige vorm: een menu met tellers, een kruimelpad en lijsten met datum, status en pijl. Er is geen eigen huisstijl of bibliotheek nodig.

### Inloggen

- Echt: met `OPENEVENTS_OIDC_ISSUER` en `OPENEVENTS_UI_CLIENT_ID` toont de app "Inloggen". Dat is authorization code met PKCE bij de IdP, met een publieke client zonder geheim. Het token gaat als `Bearer` mee, wordt stil vernieuwd en staat alleen in `sessionStorage`. Met `OPENEVENTS_ACTOR_PREFIX` is de actor-id gelijk aan de ids in de data. Op DemoStack is dat Keycloak-client `mijn-demodam`, met de claims `rol=inwoner` en `bsn`; zo ziet Suzanne ook haar zaken uit Open Zaak.
- Demo: met `OPENEVENTS_DEV_AUTH=true` kies je rechtsboven een persona uit de profielen ("bekijk als").

Vergeleken met de lab-demo (gemeten lokaal, als Jan, met MijnZaken en MijnTaken op OpenEvents en de andere API's als mock):

| | Lab-demo | Mijn Demodam |
|---|---|---|
| Verzoeken bij laden | 21 in de dev-server (React doet ze daar dubbel), dus zo'n 11 naar 9 API's | 5: wie, profielen, één query, ticket, stroom |
| Live bijwerken | lijsten opnieuw ophalen (4 verzoeken in een minuut met de simulator aan) | zit in de stroom: 0 extra verzoeken in die minuut |
| Iets doen | per API een eigen endpoint | altijd `POST /events` |
| Code | `App.tsx` 3.735 regels TypeScript, plus build | ongeveer 340 regels JavaScript, inloggen incluis |

De lab-demo doet veel meer (producten, agenda, plannen, een assistent), dus de regels zijn geen eerlijke vergelijking van functies. Het punt is de vorm: één stroom en één schrijfroute in plaats van een client per API.
