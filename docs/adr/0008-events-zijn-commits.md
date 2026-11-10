# ADR-0008: Events zijn commits, het type benoemt de handeling

- **Status:** Aangenomen (Joep, 10 november 2026)
- **Datum:** 2026-11-10

## Context

Tot nu toe had OpenEvents drie manieren van schrijven die op elkaar leken:

1. **Domein-events** (`nl.vng.gesprekken.bijdrage.toegevoegd`) met een eigen dataschema en een projectie per type (`set`, `merge`, `append`, `remove`, `increment`, paden met `{$data.x}`) die de stand berekent.
2. **JSON Commit** (`json.commit`, `json.commit.transaction`): generiek, "resource X ziet er nu zo uit" of een patch daarop.
3. **De ZGW-brug**, die notificaties omzette naar domein-events met de opgehaalde resource erin, en daarna een delta moest gaan berekenen.

Een eventtype deed twee dingen tegelijk: zeggen *wat er veranderde* en zeggen *waarom, en of dat mag*. De projectietaal was het lastigste en foutgevoeligste deel van de kern, en elk profiel moest per type een projectie schrijven. Platform Dienstverlening vraagt om handelingsgedreven API's: atomaire handelingen op een register, zonder nieuwe API-laag.

## Beslissing

**Elk event is een commit (of een transactie van commits). Het CloudEvent-`type` benoemt de handeling.**

- `data` is altijd een JSON Commit: `resourceId`, `type` (bij aanmaken), en precies een van `resourceData` (aanmaken), `patch` (JSON Merge Patch, RFC 7396) of `deleted: true`. Meerdere resources tegelijk: `data.commits`, alles of niets.
- De stand van een resource is het toepassen van de commits. Er zijn geen projecties meer.
- Een profiel beschrijft **types** (schema, regels, velden per rol) en **handelingen** (`actions`). Een handeling zegt welke commits erin mogen: welk type, aanmaken/wijzigen/verwijderen, welke velden de client mag zetten, welke velden de server zet (`set`, alleen vaste waarden, `$actor` en `$time`), en optioneel een eigen regel wie dit mag.
- Een kale `json.commit` (zonder handeling) mag alleen bij types met `allowCommits`.
- Regels en filters mogen een verwijzing volgen: `gesprek.deelnames.partij = actor()` op een bijdrage leest het gesprek waar `gesprek` naar wijst. In client-filters alleen als de lezer dat onderwerp mag zien.
- Lijsten die groeien (bijdragen, statussen) worden eigen resources met een verwijzing naar hun ouder. Kleine lijsten die zelden veranderen (deelnames) blijven een veld; de client stuurt dan de hele lijst, met `previousCommit` tegen gelijktijdig schrijven.
- In de log staat per gewijzigde resource een event met het type van de handeling, de resource als `subject` en de commit als `data`.

## Voorbeeld: een bijdrage aan een gesprek

**Was** (domein-event met projectie):

```json
{ "type": "nl.vng.gesprekken.bijdrage.toegevoegd", "subject": "urn:uuid:gesprek-1",
  "data": { "tekst": "Wanneer komt de monteur?" } }
```

met in het profiel een projectie die `bijdragen` aanvult, `laatsteBijdrage` zet en `aantalBijdragen` ophoogt, en recht `append` met regel `deelnames.partij = actor() AND status = 'open'`.

**Wordt** (handeling met commit):

```json
{ "type": "nl.vng.gesprekken.bijdrage.toegevoegd", "subject": "urn:uuid:bijdrage-7",
  "data": { "type": "nl.vng.gesprekken.bijdrage",
            "resourceData": { "gesprek": "urn:uuid:gesprek-1", "tekst": "Wanneer komt de monteur?" } } }
```

met in het profiel:

```json
"nl.vng.gesprekken.bijdrage.toegevoegd": {
  "commits": [ { "type": "nl.vng.gesprekken.bijdrage", "op": "create",
                 "fields": ["gesprek", "tekst", "automatisch"],
                 "set": { "door": "$actor", "tijd": "$time" },
                 "rule": "gesprek.deelnames.partij = actor() AND gesprek.status = 'open'" } ] }
```

`aantalBijdragen` en `laatsteBijdrage` worden een query (`type = bijdrage AND gesprek = …`). Een gesprek afsluiten is een `update` met `"set": { "status": "afgesloten" }`, zodat de client de status niet zelf hoeft te sturen.

## Alternatieven

- **Domein-events met projecties houden.** Rijk, maar elk profiel schrijft een kleine programmeertaal, en de ZGW-brug en JSON Commit blijven aparte paden.
- **Alleen kale JSON Commits.** Het eenvoudigst, maar dan is er geen plek voor rechten per handeling en geen betekenis voor consumers ("aanvulling gevraagd" is meer dan een patch).
- **JSON Patch (RFC 6902) in plaats van Merge Patch.** Kan lijsten aanvullen, maar is lastiger te lezen en te valideren. Groeiende lijsten worden hier eigen resources, dus Merge Patch is genoeg.

## Gevolgen

- De kern wordt kleiner: de projectietaal verdwijnt; domein-events, JSON Commit en de ZGW-brug zijn één pad.
- Profielen worden korter: schema, regels en handelingen.
- Clients schrijven explicieter: ze sturen de nieuwe resource of de patch. Wat de server zet (`door`, `tijd`, een vaste status) staat in `set`.
- De query-engine moet verwijzingen volgen (nu in regels en filters; `include` volgt). Dat was al nodig voor lab issue #16.
- Afgeleide velden (tellingen, "laatste") zijn een query, of een module die ze schrijft.
- Meer kleine resources: beter voor wissen per resource (AVG) en minder conflicten.
- Bestaande data in de oude vorm wordt niet omgezet; dit is een vroege fase.
