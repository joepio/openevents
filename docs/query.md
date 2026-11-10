# Query-engine

OpenEvents is de plek waar één krachtige `/query` kan bestaan. [ADR-0003 van het lab](https://github.com/VNG-Realisatie/vng-api-lab/blob/main/architecture/adr/0003-begrensde-query-grammatica.md) begrensde de taal vooral omdat elke leverancier hem zelf moest bouwen. Als één component het doet, kan de taal rijker worden. De grenzen die autorisatie en kostenbeheersing mogelijk maken, blijven wel. Zie [ADR-0003 van dit repo](adr/0003-query-krachtig-maar-begrensd.md).

**Query en autorisatie zijn één ding.** Er is één filtertaal. Een client gebruikt hem om te vragen wat hij wil zien, een abonnement om te zeggen wat het wil volgen, en een profiel om te zeggen wat iemand mag zien. Het register voert altijd `filter van de client AND regel uit het profiel` uit. Zie [autorisatie](autorisatie.md).

## Vorm van een query

```json
POST /query
Content-Type: application/json

{
  "type": "nl.vng.gesprekken.gesprek",
  "filter": {
    "all": [
      { "status": "open" },
      { "aanleiding.zaak.zaaktype": "https://catalogi.example.nl/zaaktypen/wmo-1" }
    ]
  },
  "fields": ["onderwerp", "status", "laatsteBijdrage"],
  "include": {
    "deelnames": { "fields": ["rol", "partij"] },
    "aanleiding.zaak": { "fields": ["identificatie", "omschrijving"] }
  },
  "sort": [{ "field": "laatsteBijdrage.tijd", "order": "desc" }],
  "pageSize": 20,
  "cursor": null
}
```

Antwoord:

```json
{
  "results": [ { "id": "...", "type": "...", "onderwerp": "...", "deelnames": [ ... ], "aanleiding": { "zaak": { ... } } } ],
  "next": "opaque-cursor",
  "feedCursor": "1840221"
}
```

## Filtertaal

De basis is de filtertaal uit [lab PR #53](https://github.com/VNG-Realisatie/vng-api-lab/pull/53), met de dialecten van de [CloudEvents Subscriptions API](https://github.com/cloudevents/spec/blob/main/subscriptions/spec.md). Een filter is een JSON-object:

| Vorm | Betekenis |
|---|---|
| `{ "status": "open" }` | korte vorm van `exact`: veld is gelijk aan waarde |
| `{ "exact": { "status": "open" } }` | idem, lange vorm |
| `{ "prefix": { ... } }`, `{ "suffix": { ... } }` | begint of eindigt met |
| `{ "in": { "status": ["open", "wacht"] } }` | een van de waarden |
| `{ "gte": { ... } }`, `{ "lte": { ... } }` | groter of gelijk, kleiner of gelijk |
| `{ "exists": "einddatum" }` | veld heeft een waarde |
| `{ "all": [ ... ] }`, `{ "any": [ ... ] }`, `{ "not": { ... } }` | en, of, niet |
| `{ "text": { "onderwerp": "parkeervergunning" } }` | vrije tekst, alleen op velden die het profiel als doorzoekbaar markeert |

En twee dingen die het filter krachtig maken:

| Element | Betekenis |
|---|---|
| **pad** in een veldnaam, zoals `aanleiding.zaak.zaaktype` | volg verwijzingen tot het veld; maximaal 3 stappen. Over een lijst betekent het "een van de elementen", dus `deelnames.partij` matcht als een deelname die partij heeft. |
| **variabele** `$actor`, `$actor.<claim>` | wie er vraagt, uit het token. Alleen in regels van het profiel, nooit in een filter van een client. |

Alleen velden die het profiel als `queryable` markeert, mogen in een filter of sortering staan. Zo weet de engine dat er een index voor is, en weet de beheerder wat er bevraagbaar is.

Dezelfde taal werkt op `GET /events` (een abonnement) en in de regels van een profiel.

## Paden over verwijzingen

Dit is wat ZGW vandaag niet kan, en waar de G4 last van heeft: "zaken waar ik betrokkene ben" vraagt nu om een filter op een gerelateerde Rol, of om alles op te halen en in het portaal te filteren.

De engine houdt bij elke verwijzing in een projectie twee indexen bij: vooruit (`idx_field` op het verwijzende veld) en achteruit (`idx_ref`). Een pad wordt van de meest selectieve kant uitgevoerd. "Gesprekken bij zaken van zaaktype X" wordt dus: zaken met zaaktype X uit `idx_field`, daarna via `idx_ref` de gesprekken die ernaar verwijzen.

Paden zijn ook wat autorisatie simpel maakt: "gesprekken bij zaken waarvan het zaaktype bij een team hoort waar ik lid van ben" is `aanleiding.zaak.zaaktype.team.leden = $actor`. Zie [autorisatie](autorisatie.md).

Een pad kan alleen over onderwerpen die in hetzelfde register staan. Verwijzingen naar buiten (een zaak in Open Zaak) kunnen meedoen als het register een **spiegel** van die bron bijhoudt: een profiel dat de events van die bron volgt en een kleine projectie bewaart (bijvoorbeeld zaak-URL, zaaktype en betrokkenen). Dat is precies het "per-domein register" uit lab ADR-0006 en de ingest uit lab ADR-0004.

## Representaties

Een representatie is wat je terugkrijgt: welke velden, en welke gerelateerde resources erin zijn opgenomen.

- `fields` kiest velden. Zonder `fields` krijg je de standaardvelden van het profiel.
- `include` neemt gerelateerde resources mee, met hun eigen `fields` en een maximum aantal per relatie. Dit lost het N+1-probleem op dat Rutger aankaartte bij de weergavegegevens.
- De velden die je mag zien, komen uit dezelfde regel als de rijen die je mag zien. Wat je niet mag zien, staat er niet in, ook niet in een `include`, en ziet er hetzelfde uit als een veld zonder waarde.

### Benoemde representaties (views)

Een view is een query-template met parameters, geregistreerd door een beheerder of meegeleverd met een profiel:

```json
PUT /views/mijn-open-gesprekken
{
  "query": {
    "type": "nl.vng.gesprekken.gesprek",
    "filter": { "status": "open" },
    "fields": ["onderwerp", "status", "laatsteBijdrage", "ongelezen"],
    "sort": [{ "field": "laatsteBijdrage.tijd", "order": "desc" }]
  }
}
```

De view hoeft niet te zeggen "van mij": de leesregel van het profiel doet dat al. Een inwoner die deze view aanroept, krijgt alleen zijn eigen gesprekken.

Views zijn de domein-templates uit lab ADR-0003 ("WOZ, Parkeren, Belasting zijn query-templates bovenop deze grammatica"). Ze zijn:

1. **Cachebaar.** `GET /views/{naam}?...` mag als alle parameters niet-gevoelig zijn. Met een BSN of andere gevoelige waarde gebruik je `POST /views/{naam}`.
2. **Een stabiel contract.** Een portaal leunt op `mijn-open-gesprekken`, niet op de interne velden van het profiel.
3. **Een grens.** Een profiel kan per rol zeggen dat alleen views mogen, geen vrije query's.

## Live bevragen

Elke query-respons bevat een `feedCursor`: het volgnummer van het log, vastgesteld vóór de snapshot wordt gelezen (zoals `patterns/sync-feed` voorschrijft). Een client die daarna `GET /events` opent met hetzelfde filter en `Last-Event-ID: <feedCursor>`, mist niets en krijgt niets dubbel. Dit is het patroon snapshot plus SSE uit lab ADR-0002, en het antwoord van dit ontwerp op de "live query" uit [lab issue #16](https://github.com/VNG-Realisatie/vng-api-lab/issues/16), zonder WebSockets.

Voor filters met paden is "dezelfde selectie volgen" lastiger: een gesprek kan in de selectie komen doordat de zaak van zaaktype verandert, of doordat jij lid wordt van een team. De engine houdt daarom per open stroom bij welke onderwerpen erin zitten, en stuurt `subject.matched` of `subject.unmatched` als dat verandert. Dat is hetzelfde mechanisme voor een gewijzigd filter als voor gewijzigde rechten.

## HTTP-methode

- `POST /query` met een JSON-body is de standaard. Persoonsgegevens staan dan niet in URL's en logs (eis A8).
- Zodra clients de HTTP-methode [`QUERY`](https://httpwg.org/http-extensions/draft-ietf-httpbis-safe-method-w-body.html) ondersteunen, accepteert hetzelfde pad dezelfde body met `QUERY`. Die is veilig en idempotent, en dus cachebaar.
- `GET` alleen voor views met niet-gevoelige parameters.

## Grenzen en kosten

Elke query krijgt vóór uitvoering een kostenschatting op basis van de indexen. De leesregel telt mee. Vaste grenzen in de eerste versie:

| Grens | Waarde |
|---|---|
| Diepte van filters | 3 niveaus |
| Elementen per `all` of `any` | 10 |
| Waarden in `in` | 50 |
| Stappen in een pad | 3 |
| `include`-niveaus | 2 |
| Resultaten per `include`-relatie | 50 |
| `pageSize` | 100 |

Is de schatting te hoog, dan komt een `problem+json` met type `query-too-expensive` terug, met daarin welke grens werd geraakt.

## Tellingen

Een `total` is optioneel en altijd binnen de rechten berekend. Is dat te duur, dan geeft de engine `"total": { "atLeast": 1000 }`. Tellingen buiten de rechten bestaan niet (eis A3).

## Wat er bewust niet in zit

- Geen vrije joins over willekeurige velden; alleen paden over verwijzingen die het profiel kent.
- Geen aggregaties in de eerste versie (gemiddelden, groeperen). Die zijn een aparte autorisatievraag.
- Geen GraphQL, OData, Elasticsearch-DSL of JSON Logic. De redenen staan in lab PR #53. GraphQL blijft een kandidaat voor een tweede ingang op dezelfde engine.
- **Open keuze:** CQL2-JSON (OGC), genoemd in lab issue #51, is een alternatief voor de CloudEvents-dialecten. Het is rijker, maar deelt geen taal met CloudEvents-abonnementen. Zolang de engine een geparste boom gebruikt, is de syntaxis later nog te wisselen.
