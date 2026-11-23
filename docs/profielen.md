# Profielen

Een profiel is een domein op het register. Het is data, geen code, en wordt tijdens runtime geregistreerd (eis F10). Een profiel beschrijft twee dingen: **types** (hoe een resource eruitziet en wie hem mag zien) en **handelingen** (welke wijzigingen een client mag sturen). Zie [ADR-0008](adr/0008-events-zijn-commits.md).

## Events zijn commits

Elk event is een commit: "maak deze resource", "verander deze velden" of "verwijder deze resource". Het CloudEvent-`type` benoemt de handeling, de `data` is de commit.

```json
{ "specversion": "1.0", "id": "…", "source": "https://mijn.demodam.nl",
  "type": "nl.vng.gesprekken.bijdrage.toegevoegd", "subject": "urn:uuid:bijdrage-7",
  "data": { "resourceData": { "gesprek": "urn:uuid:gesprek-1", "tekstGespreksBijdrage": "Wanneer komt de monteur?" } } }
```

`data` heeft `resourceId` (standaard het `subject`), bij een nieuwe resource het `type`, en precies een van:

- `resourceData`: de nieuwe resource;
- `patch`: wat er verandert, als JSON Merge Patch (RFC 7396): een veld met `null` verdwijnt, een object wordt samengevoegd, een lijst wordt vervangen;
- `deleted: true`.

Optioneel `previousCommit`: de laatste commit die de client zag. Klopt die niet meer, dan volgt 412. Meerdere resources tegelijk gaan in `data.commits`, alles of niets. In het log staat per gewijzigde resource een event, met het type van de handeling, de resource als `subject` en de commit als `data`. De stand van een resource is zijn commits na elkaar toegepast. Er zijn geen projectieregels.

## Wat een profiel bevat

```json
{
  "id": "nl.vng.gesprekken",
  "version": "0.3.0",
  "types": {
    "nl.vng.gesprekken.gesprek": {
      "schema": { "allOf": [
        { "$ref": "https://raw.githubusercontent.com/VNG-Realisatie/vng-api-lab/main/schemas/gesprekken/next.json#/$defs/Gesprek" },
        { "properties": { "team": { "type": "string" }, "afsluitreden": { "type": "string" } } }
      ] },
      "queryable": ["gespreksonderwerp", "team", "aanleidingZaak", "gespreksdeelnames.partij", "momentEindeGesprek"],
      "references": ["aanleidingZaak"],
      "rules": {
        "read":   "gespreksdeelnames.partij = actor() OR gespreksdeelnames.actor = actor() OR team = actor('team') OR actor('rol') = 'kcc'",
        "write":  "team = actor('team')",
        "fields": [ { "when": "actor('rol') = 'kcc'", "show": ["gespreksonderwerp", "aanleidingZaak", "team", "momentEindeGesprek"] } ]
      }
    },
    "nl.vng.gesprekken.bijdrage": {
      "schema": { "$ref": "https://raw.githubusercontent.com/VNG-Realisatie/vng-api-lab/main/schemas/gesprekken/next.json#/$defs/Gespreksbijdrage" },
      "references": ["gesprek"],
      "rules": { "read": "gesprek.gespreksdeelnames.partij = actor() OR gesprek.gespreksdeelnames.actor = actor() OR gesprek.team = actor('team')" }
    }
  },
  "actions": {
    "nl.vng.gesprekken.bijdrage.toegevoegd": {
      "title": "Bijdrage plaatsen",
      "commits": [ {
        "type": "nl.vng.gesprekken.bijdrage", "op": "create",
        "fields": ["gesprek", "tekstGespreksBijdrage", "isReactieOp", "bijlagen", "automatisch"],
        "set": { "verzender": "$actor", "leveringsmoment": "$time" },
        "rule": "(gesprek.gespreksdeelnames.partij = actor() OR gesprek.gespreksdeelnames.actor = actor() OR gesprek.team = actor('team')) AND gesprek.momentEindeGesprek IS NULL"
      } ]
    },
    "nl.vng.gesprekken.gesprek.afgesloten": {
      "commits": [ {
        "type": "nl.vng.gesprekken.gesprek", "op": "update",
        "fields": ["afsluitreden", "contactmoment"],
        "set": { "momentEindeGesprek": "$time" },
        "rule": "(gespreksdeelnames.partij = actor() OR gespreksdeelnames.actor = actor() OR team = actor('team')) AND momentEindeGesprek IS NULL"
      } ]
    }
  }
}
```

Het volledige voorbeeld staat in `examples/profiles/gesprekken.json`.

### Types

- `schema`: JSON Schema van de stand. Elke commit wordt daartegen gecontroleerd, na het toepassen. Liefst een verwijzing naar een gedeeld schema, zie hieronder.
- `queryable`: velden waarop een client mag filteren. Een veld dat naar een andere resource verwijst (zoals `gesprek`), maakt ook paden erdoorheen mogelijk (`gesprek.status`).
- `rules`: wie mag lezen (`read`), aanmaken (`create`) en wijzigen (`write`), en welke velden je ziet (`fields`). Zie [autorisatie](autorisatie.md).
- `allowCommits: true`: dit type accepteert ook een kale `json.commit` zonder handeling, met alleen de regels van het type. Standaard staat dat uit: een domein met betekenis werkt met handelingen.

### Gedeelde schema's

Hoe een gesprek of bijdrage eruitziet, hoort niet in het profiel maar op één gedeelde plek, het VNG API Lab (`schemas/{domein}/next.json`). Het profiel verwijst ernaar met `$ref` en voegt toe wat de lab niet heeft: de handelingen, de regels, de verwijzingen en eventueel eigen velden (met `allOf`, zoals `team` hierboven).

Bij het registreren haalt OpenEvents de schema's op en slaat het profiel op met die schema's erin, plus de bron-URL's in `schemaSources`. Daarna heeft het register het netwerk niet meer nodig, en komt een wijziging in de lab pas binnen met een nieuwe profielversie. Een lokale kopie gaat voor: met `OPENEVENTS_SCHEMAS=<map>` leest OpenEvents `https://host/pad` uit `<map>/host/pad`. De voorbeelden gebruiken `examples/schemas`, met een kopie van het lab-schema voor Gesprekken.

### Handelingen

Een handeling (`actions`) noemt welke commits erin mogen. Per commit:

- `type` en `op` (`create`, `update` of `delete`);
- `fields`: de velden die de client mag zetten. Een ander veld geeft 422 `field-not-allowed`;
- `set`: velden die het register zelf zet, alleen vaste waarden, `$actor` en `$time`. Zo hoeft een client de status niet te sturen, en kan hij `door` niet vervalsen;
- `rule`: wie dit mag. Een `create` wordt beoordeeld op de nieuwe resource, een `update` of `delete` op de stand ervoor. Zonder `rule` gelden de `create`- en `write`-regel van het type.

Een handeling met meerdere commits is één transactie: "gesprek starten" maakt het gesprek en de eerste bijdrage in één keer. De regel van de bijdrage ziet dan het gesprek uit dezelfde transactie.

### Verwijzingen

Een regel of filter mag een verwijzing volgen: in `gesprek.gespreksdeelnames.partij` is `gesprek` een veld met de id van een andere resource, en het register leest die resource. Regels in het profiel zien alles. Een filter van een client volgt een verwijzing alleen als de lezer die resource zelf mag lezen, en ziet daarvan alleen de velden die hij mag zien. Een pad volgt hooguit drie verwijzingen.

Een type noemt zijn verwijzingen in `references` (bijvoorbeeld `["gesprek"]` bij een bijdrage, `["zaak"]` bij een taak of een ZGW-status). Daarop kun je altijd filteren, en het register gebruikt ze voor twee leesvragen:

- `GET /subjects/{subject}/related`: alles wat naar dit onderwerp verwijst (de gesprekken, taken en aanvullingen bij een zaak);
- `GET /subjects/{subject}/audit`: één tijdlijn van wie wat wanneer deed, over het onderwerp en alles wat ernaar verwijst, twee lagen diep (de zaak, het gesprek erover, de bijdragen daarin).

Beide volgen de leesregels van de lezer, net als `/query` en de stroom: een inwoner ziet de bijdragen in zijn gesprek, het KCC ziet dat er iets gebeurde maar niet wat. Is de verwijzing verplicht in het schema, dan hoort het type bij iets anders (een bijdrage bij een gesprek); de andere types staan op zichzelf en komen in een overzicht (`standalone` in `GET /profiles`). Het tabblad Overzicht in `/ui` laat dat per persona zien: taken, zaken, aanvragen, meldingen en gesprekken, live, met per onderwerp wat erbij hoort en de audit.

### Lijsten

Een lijst die groeit (bijdragen, statussen, aanvullingen, adviezen) wordt een eigen resource met een verwijzing naar zijn ouder. Dat geeft minder conflicten, kleinere events en wissen per resource. Een kleine lijst die zelden verandert (deelnames) blijft een veld. De client stuurt dan de hele lijst, met `previousCommit` tegen gelijktijdig schrijven.

Afgeleide velden zoals "aantal bijdragen" of "laatste bijdrage" zijn een query (`type = bijdrage AND gesprek = …`), of een consumer die ze schrijft.

## Reacties horen bij consumers

Regels die iets laten gebeuren, zijn geen onderdeel van het profiel maar kleine consumers naast het register. Ze volgen de stroom met een eigen cursor en schrijven gewone handelingen terug:

- het automatische antwoord "binnen 2 werkdagen" op een bijdrage van een inwoner, met `automatisch: true`
- een gesprek sluiten na 3 dagen zonder bijdrage van de gemeente
- een klantcontact in Open Klant aanmaken bij afsluiten

Een consumer is een gewone `actor()` die door de regels van de handeling moet. Zo blijft de geschiedenis compleet, en is elk automatisch event te herleiden tot wie het schreef.

## Versies

Een profiel heeft een versie. Een nieuwe profielversie mag velden en handelingen toevoegen. Velden weghalen of de betekenis van een handeling veranderen vraagt een nieuwe handeling. Omdat elk event een commit is, blijft het log altijd af te spelen, ook als het profiel later verandert.

`GET /profiles/{id}/openapi.json` geeft per handeling het schema van de commits, gegenereerd uit het profiel.

## Eerste profiel: Gesprekken

Het eerste echte profiel is MijnGesprekken, omdat daar een werkende PoC ligt (OneGround-fork, Open Inwoner-fork, demo op DemoStack).

| In de Gesprekken API (lab PR #47) | In OpenEvents |
|---|---|
| `POST /gesprekken` | handeling `gesprek.gestart` (gesprek, eventueel met eerste bijdrage) |
| `POST /gesprekken/{id}/bijdragen` | handeling `bijdrage.toegevoegd`, een bijdrage als eigen resource |
| `GET /gesprekken?...` | `/query` op `nl.vng.gesprekken.gesprek` |
| `GET /gesprekken/{id}/bijdragen` | `/query` met `gesprek = '…'` |
| `GET /events` (SSE) | `GET /events` met filter op het profiel |
| deelnames en rechten | veld `deelnames` plus regels die verwijzingen volgen |
| gelezen | handeling `bijdrage.gelezen` (nog niet in het voorbeeld) |
| automatisch antwoord | consumer die op `bijdrage.toegevoegd` reageert |
| automatisch sluiten na 3 dagen | consumer met een timer |
| klantcontact bij afsluiten | consumer die `gesprek.afgesloten` volgt en Open Klant aanroept |
| bijlagen | `dataref` naar het file-upload-pattern |

**Voor de pilot** blijft het REST-contract van Gesprekken bestaan, want Roxit en Maykin verwachten dat. Er zijn dan twee routes:

1. **Naast:** de Gesprekken API (OneGround-fork) publiceert zijn wijzigingen naar OpenEvents. De medewerkerconsole en Open Inwoner lezen via OpenEvents.
2. **Erop:** een dunne REST-laag vertaalt het Gesprekken-contract naar handelingen op OpenEvents. Dan is OpenEvents de bron en verdwijnt de eigen opslag van de Gesprekken API.

Als route 2 werkt, is de conclusie dat een domein als Gesprekken een profiel kan zijn in plaats van een register.
