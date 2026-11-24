# Aansluiten als leverancier

Hoe een systeem van een leverancier of gemeente events naar OpenEvents stuurt. Er zijn twee routes: zelf CloudEvents posten, of niets veranderen en de Notificaties-adapter gebruiken.

## Route 1: zelf events posten

1. **Profiel.** Kies het profiel waar je events onder vallen (bijvoorbeeld `nl.vng.meldingen`). `GET /profiles/{id}/openapi.json` geeft een OpenAPI 3.1-document met per handeling (eventtype) het schema van de commits in `data`, gegenereerd uit het profiel. Valideer daar in je eigen tests tegen.
2. **Token.** Vraag een client aan bij de IdP van de gemeente (OAuth2 client credentials). OpenEvents controleert het token met de sleutels uit de JWKS van die IdP. Je systeem krijgt een claim waarmee de regels van het profiel het herkennen, bijvoorbeeld `rol=systeem`.
3. **Posten.** `POST /events` met één CloudEvent waarvan `data` een commit is (`resourceData` voor iets nieuws, `patch` voor wat er verandert, zie [profielen](profielen.md)), of met een array (`application/cloudevents-batch+json`, maximaal 500). In een batch wordt elk event apart verwerkt; het antwoord geeft per event de uitkomst (200 als alles goed ging, 207 als er iets is geweigerd).
4. **Opnieuw proberen is veilig.** `source` + `id` is idempotent: hetzelfde event nog een keer sturen geeft het eerdere antwoord terug (header `Idempotent-Replayed`). Kies dus vaste id's, bijvoorbeeld afgeleid van je eigen sleutel.
5. **Fouten.** Een 422 bevat `invalidParams` met het precieze pad (`/data/locatie`) en een code (`schema`, `field-not-allowed`, `exists`, `not-found`). Er wordt dan niets opgeslagen.

Wat je moet weten:

- **Volgorde.** Een `create` maakt een resource aan; een `update` voor een resource die nog niet bestaat, wordt geweigerd (`not-found`). Stuur bij een eerste aansluiting dus eerst de bestaande zaken in (een batch met `create`-handelingen), en daarna de stroom.
- **Stuur de wijziging.** Een `patch` bevat alleen wat er veranderde. Velden die het profiel zelf zet (`set`, zoals een status of `door`), stuur je niet mee.
- **Alles of niets** over meerdere resources is één handeling met `data.commits`, niet de batch.

## Route 2: niets veranderen, via ZGW-notificaties

Systemen die nu al ZGW-notificaties versturen (Open Zaak via Open Notificaties) hoeven niets te veranderen, en Open Notificaties ook niet. OpenEvents meldt zich zelf aan als gewone abonnee (`OPENEVENTS_NOTIFICATIES_API`, `OPENEVENTS_NOTIFICATIES_KANALEN`, `OPENEVENTS_PUBLIC_URL`) en kijkt elke vijf minuten of het abonnement er nog is, zodat het ook een reset van Open Notificaties overleeft. Het enige wat OpenEvents nodig heeft is een client-id en secret die Open Zaak en Open Notificaties al kennen.

Per notificatie haalt OpenEvents de resource op (met een ZGW-JWT, `OPENEVENTS_ZGW_CLIENT_ID` en `OPENEVENTS_ZGW_SECRET`), vergelijkt die met wat het al had, en slaat de wijziging op als handeling:

- type `nl.vng.zgw.<kanaal>.<resource>.<actie>`, bijvoorbeeld `nl.vng.zgw.zaken.status.create`;
- subject: de resource zelf (de status-URL); elke ZGW-resource is een eigen resource van type `nl.vng.zgw.<resource>`, met een verwijzing `zaak`;
- data: een commit. Nieuw is `resourceData`, gewijzigd is een `patch` met alleen wat veranderde, `destroy` is `deleted: true`. Verandert er niets, dan wordt er niets opgeslagen.

Bij een subresource haalt de adapter ook de zaak opnieuw op, want Open Zaak zet dan bijvoorbeeld `status` of `resultaat` op de zaak. Die wijziging gaat als tweede commit in dezelfde handeling. Bij een rol `initiator` zet de adapter ook het BSN op de zaak (`initiatorBsn`), zodat de leesregel van de zaak het kan gebruiken.

Alleen handelingen die een profiel registreert worden opgeslagen; de rest wordt bevestigd en genegeerd. Het voorbeeldprofiel `examples/profiles/zgw-zaken.json` maakt er een zaakspiegel van: zaak, statussen, rollen, resultaat, documenten en objecten. Leesregel: een medewerker ziet de zaaktypen in zijn token (`zaaktype = actor('zaaktypen')`), het KCC een beperkte versie, de initiator zijn eigen zaak (`initiatorBsn = actor('bsn')`). Een status of rol volgt de zaak: `zaak.zaaktype = actor('zaaktypen')`.

Volgorde lost de adapter zelf op: komt er een notificatie voor een zaak die OpenEvents nog niet kent, dan haalt hij eerst de zaak op en slaat die op als `…zaak.create`. Een notificatie die twee keer binnenkomt, wordt één keer opgeslagen (de event-id is afgeleid van de notificatie). Bij een fout antwoordt OpenEvents met 502, zodat Open Notificaties het later opnieuw probeert.

Nog niet: vertrouwelijkheidaanduiding in de leesregel (dat vraagt een vergelijking op volgorde van een opsomming).

## Configuratie

| Variabele | Betekenis |
|---|---|
| `OPENEVENTS_OIDC_ISSUER` | issuer van de IdP; de JWKS wordt via discovery gevonden en ververst bij een onbekende `kid` |
| `OPENEVENTS_OIDC_JWKS_URL` | JWKS-URL, als er geen discovery is |
| `OPENEVENTS_OIDC_AUDIENCE` | verwachte `aud` (aanbevolen) |
| `OPENEVENTS_ACTOR_CLAIM` | claim met de actor-id, standaard `sub`; bij client credentials vaak `azp` of `client_id` |
| `OPENEVENTS_ACTOR_PREFIX` | komt voor de claim, zodat de actor-id gelijk is aan de ids in de data (`https://id.example.nl/` + `suzanne`) |
| `OPENEVENTS_UI_CLIENT_ID` | publieke client bij de IdP voor inloggen in `/mijnoverheid` (authorization code met PKCE, geen geheim) |
| `OPENEVENTS_NOTIFICATIES_AUTH` | exacte Authorization-header die Open Notificaties meestuurt; zonder deze variabele staat `/notificaties` uit |
| `OPENEVENTS_ZGW_CLIENT_ID`, `OPENEVENTS_ZGW_SECRET` | om resources op te halen bij Open Zaak |
| `OPENEVENTS_NOTIFICATIES_API` | Notificaties API (bijvoorbeeld `https://notificaties.example.nl/api/v1`): OpenEvents abonneert zich zelf |
| `OPENEVENTS_NOTIFICATIES_KANALEN` | kanalen voor het abonnement, komma's, standaard `zaken` |
| `OPENEVENTS_IMPORT_ZAKEN` | Zaken API (`https://…/zaken/api/v1`): bij het opstarten haalt OpenEvents alle zaken op, met rollen, statussen, resultaat en documenten, alsof er voor elk een notificatie kwam. Zo staan ook zaken van vóór het abonnement in de spiegel. Opnieuw draaien verandert niets. |
| `OPENEVENTS_PUBLIC_URL` | publieke basis-URL van OpenEvents, voor de callback |
| `OPENEVENTS_NOTIFICATIES_ACTOR` | actor-id van de adapter, standaard `urn:openevents:notificaties` (claim `rol=systeem`) |
| `OPENEVENTS_ZGW_SOURCES` | meer ZGW-bronnen naast de eerste, bijvoorbeeld `og` voor een tweede zaakregister (OneGround). Elke bron heeft dezelfde variabelen met een eigen voorvoegsel: `OPENEVENTS_OG_NOTIFICATIES_AUTH`, `OPENEVENTS_OG_ZGW_CLIENT_ID`, `OPENEVENTS_OG_ZGW_SECRET`, `OPENEVENTS_OG_NOTIFICATIES_API`, `OPENEVENTS_OG_NOTIFICATIES_KANALEN`, `OPENEVENTS_OG_IMPORT_ZAKEN`. Alle bronnen gebruiken de callback `/notificaties`; de Authorization-header zegt van welke bron een notificatie komt, dus geef elke bron een eigen geheim. Zaken uit alle registers komen in één spiegel en één `mine`-overzicht. |
| `OPENEVENTS_PSEUDONYM_KEY` | geheime sleutel: BSN's in tokens en in opgehaalde ZGW-resources worden `pseudo:`-pseudoniemen (zie [wissen.md](wissen.md)) |
| `OPENEVENTS_PSEUDONYM_CLAIMS` | welke claims een pseudoniem krijgen, komma's, standaard `bsn` |
| `OPENEVENTS_BACKUP_DIR`, `OPENEVENTS_BACKUP_HOURS` | consistente backup elke n uur (standaard 24), laatste zeven bewaard (zie [beheer.md](beheer.md)) |
| `OPENEVENTS_RESTORE` | backup die bij het opstarten in een lege opslag wordt geladen |
