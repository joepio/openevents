# Autorisatie

## Uitgangspunten

1. **Het onderwerp is de eenheid.** Rechten hangen aan een onderwerp (een gesprek, een zaakspiegel), niet aan losse events.
2. **Twee bronnen van rechten.** Een lijst per onderwerp (wie doet mee) en regels bij een externe beslisser (rol, zaaktype, vertrouwelijkheid).
3. **Autorisatie is een filter.** Voor lezen en bevragen levert de beslissing geen ja of nee per rij op, maar een filter en een lijst toegestane velden. Dat filter gaat met AND in de geparste query (lab issue #51).
4. **Vóór het pagineren.** Rechten worden via een index toegepast, zodat tellingen, offsets en cursors niets lekken.

## Rechten per onderwerp

Elk onderwerp heeft een ACL met de rechten `read`, `append` en `write`:

| Recht | Betekent |
|---|---|
| `read` | events en projectie lezen, onderwerp verschijnt in query's |
| `append` | nieuwe events toevoegen van types die het profiel als "toevoegen" markeert (bijvoorbeeld `bijdrage.toegevoegd`) |
| `write` | alle types, ook wijzigen van de stand (bijvoorbeeld afsluiten) |

De ACL wordt bijgehouden door de projectieregels van het profiel. Bij Gesprekken leveren de deelnames de ACL: inwoner `read` + `append`, curator `read` + `write`, bewindvoerder `read` + `write`, behandelaar `write`. Zo is de testcase van Vincent (curator lezen en schrijven, inwoner lezen; bij bewind beiden) een regel in het profiel en geen code.

Een ACL-regel kan naar een agent wijzen of naar een groep (bijvoorbeeld "team Wmo"). Groepslidmaatschap komt uit het token of uit een groepenregister, niet uit het register zelf.

## Regels via AuthZEN

Voor medewerkers werkt een lijst per onderwerp niet: zij zien wat hun rol en zaaktypen toestaan. Daarvoor vraagt het register een externe beslisser volgens [AuthZEN](https://openid.net/wg/authzen/):

```json
POST /access/v1/evaluation
{
  "subject":  { "type": "user", "id": "behandelaar-12", "properties": { "roles": ["wmo-consulent"] } },
  "action":   { "name": "query" },
  "resource": { "type": "nl.vng.gesprekken.gesprek" },
  "context":  { "register": "gesprekken.gemeente.nl" }
}
```

Antwoord, met het filter en de velden in `context` (voorstel, zie lab issue #51):

```json
{
  "decision": true,
  "context": {
    "filter": { "path": ["aanleiding", "zaak"], "filter": { "in": { "zaaktype": ["https://.../zaaktypen/wmo-1", "https://.../zaaktypen/wmo-2"] } } },
    "fields": { "exclude": ["deelnames.partij.bsn"] }
  }
}
```

Het register cachet zo'n beslissing per agent en type, zo kort als de beslisser opgeeft.

## Hoe het in de engine werkt

1. De query wordt geparst en gevalideerd tegen het profiel.
2. Het register haalt de beslissing op (ACL-tak en, als het beleid dat zegt, AuthZEN-tak).
3. De engine bouwt: `queryFilter AND (aclFilter OR beslisserFilter)`.
4. De ACL-tak loopt via `idx_readable`: per agent of groep de onderwerpen die leesbaar zijn. Die index wordt in dezelfde transactie bijgewerkt als de ACL.
5. De velden worden gemaskeerd in de representatie, in elke `include` en op de stroom.

Geen stap plakt tekst aan een query. Het filter van de beslisser wordt geparst met dezelfde parser als die van de client, met dezelfde grenzen.

## Op de stroom

Een SSE-verbinding krijgt bij het openen dezelfde beslissing. Elk event wordt tegen `idx_readable` of het beslisserfilter gehouden voordat het de deur uitgaat, en velden worden gemaskeerd. Verandert de ACL van een onderwerp (iemand wordt uit een gesprek gehaald), dan krijgt de verbinding van die agent vanaf dat moment niets meer van dat onderwerp, en een `subject.unmatched`-melding.

Een verlopen token sluit de stroom niet hard af: de server stuurt een `auth.expiring`-melding, de client haalt een nieuw ticket en gaat verder met `Last-Event-ID`.

## Schrijven

Bij schrijven is de vraag ja of nee: mag deze agent dit type op dit onderwerp schrijven? Eerst de ACL (`append` of `write`), daarna, als het beleid dat zegt, de beslisser. Een weigering is altijd één 403 zonder details, zodat je niet kunt aftasten welke onderwerpen bestaan (zoals in ZaakChat #28).

## Producenten

Systemen zoals Open Zaak of OneGround schrijven met een eigen token. Hun recht is per profiel en per `source`: OneGround mag `nl.vng.gesprekken.*` schrijven met `source` gelijk aan zijn eigen URI, en niets anders. Alleen de bron die een onderwerp heeft gemaakt, mag het wissen (lab ADR-0004).
