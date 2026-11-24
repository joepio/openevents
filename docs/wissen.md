# Wissen en bewaren

Een eventlog is onveranderlijk, maar de AVG (recht op verwijdering) en de Archiefwet (vernietigen na de bewaartermijn) vragen dat inhoud echt kan verdwijnen. Lab ADR-0002 schrapte event sourcing als publiek contract juist om die reden. OpenEvents lost het op in de opslag, zodat het log zijn voordelen houdt.

## Wat er nu is

**Definitief verwijderen.** `DELETE /subjects/{subject}` (alleen beheerders) haalt een resource weg: de staat, de indexrijen en de idempotentie-records gaan weg, en elk event van de resource wordt in het log vervangen door een lege envelop (`id`, `type`, `subject`, `time`, `sequence`, `redacted`). Zonder `data` en zonder `actor`. Volgorde en cursors blijven zo werken. Met `?cascade=true` gaat ook alles mee dat ernaar verwijst (via `references` in de profielen, tot drie niveaus diep), zoals de bijdragen van een gesprek. Het antwoord noemt alle verwijderde resources.

**Bewaartermijnen.** Een type in een profiel kan een bewaartermijn hebben:

```json
"retention": { "from": "momentEindeGesprek", "keep": "P1Y" }
```

`from` is het veld met het startmoment (RFC 3339 of een datum, punten voor geneste velden zoals `afgerond.tijd`), `keep` een ISO 8601-duur (`P1Y`, `P6M`, `P30D`, `PT12H`). Zolang het veld leeg is, loopt de termijn niet: een open gesprek blijft staan. Elk uur verwijdert de server wat verlopen is, met cascade. In de voorbeelden: gesprekken een jaar na afsluiten, taken een jaar na afronden.

**Pseudoniemen voor het BSN.** Met `OPENEVENTS_PSEUDONYM_KEY` vervangt OpenEvents een BSN door `pseudo:` plus een HMAC-SHA256 met die sleutel, op twee plekken: in de claims van het token (standaard de claim `bsn`, te kiezen met `OPENEVENTS_PSEUDONYM_CLAIMS`) en in wat de ZGW-adapter ophaalt (de velden `bsn` en `inpBsn`). Regels als `betrokkenen.bsn = actor('bsn')` werken gewoon, want beide kanten worden hetzelfde pseudoniem. Het echte BSN komt zo niet in de spiegel. Wie de sleutel wisselt, moet de spiegel opnieuw opbouwen.

## Wat nog niet kan

- Backups bevatten de inhoud nog wel. Crypto-shredding (een sleutel per onderwerp, wissen is de sleutel vernietigen) blijft de weg om ook backups na een korte termijn schoon te krijgen (eis W1).
- Een bewaartermijn die het bovenliggende object volgt (een gesprek zo lang als zijn zaak, zoals Vincent op 5 november bevestigde) kan nog niet; nu telt alleen een veld van de resource zelf.
- Namen en adressen in rollen van Open Zaak staan nog in de spiegel. Een volgende stap is dat de adapter alleen overneemt wat een profiel nodig heeft.
- Er gaat nog geen tombstone over de stroom; zie hieronder.
- Archiveren (overbrengen naar een e-depot) is een consumer: die leest de tijdlijn vóór de vernietiging en schrijft een bewijs terug als event.

## Abonnees

Het tombstone-event gaat over de stroom zoals elk ander event. Consumers die een kopie hebben (een portaal-cache, een zoekindex) moeten daarop hun kopie wissen. Dat is onderdeel van de conformance-tests voor consumers.

## Gevoelige velden

Gevoelige velden (BSN, adres) horen niet in events als het ook met een verwijzing kan: een deelname verwijst naar een Partij in Open Klant. Waar een gevoelig veld toch nodig is, markeert het profiel het als `sensitive`. Zo'n veld wordt standaard gemaskeerd op de stroom en alleen getoond aan wie het expliciet in `fields` vraagt en er recht op heeft.
