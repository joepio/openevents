# Query-engine

OpenEvents is de plek waar één krachtige `/query` kan bestaan. [ADR-0003 van het lab](https://github.com/VNG-Realisatie/vng-api-lab/blob/main/architecture/adr/0003-begrensde-query-grammatica.md) begrensde de taal vooral omdat elke leverancier hem zelf moest bouwen. Als één component het doet, kan de taal rijker worden. De grenzen die autorisatie en kostenbeheersing mogelijk maken, blijven wel. Zie [ADR-0003 van dit repo](adr/0003-query-krachtig-maar-begrensd.md).

## Vorm van een query

```json
POST /query
Content-Type: application/json

{
  "type": "nl.vng.gesprekken.gesprek",
  "filter": {
    "all": [
      { "exact": { "status": "open" } },
      { "path": ["aanleiding", "zaak"], "filter": { "exact": { "zaaktype": "https://.../zaaktypen/123" } } }
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

De basis is de filtertaal uit [lab PR #53](https://github.com/VNG-Realisatie/vng-api-lab/pull/53), die op zijn beurt de dialecten van de [CloudEvents Subscriptions API](https://github.com/cloudevents/spec/blob/main/subscriptions/spec.md) gebruikt: `exact`, `prefix`, `suffix`, `all`, `any`, `not`, aangevuld met `in`, `gte` en `lte`. Dezelfde taal werkt op `GET /events`, zodat een query en een abonnement precies dezelfde selectie beschrijven.

OpenEvents voegt toe:

| Operator | Betekenis |
|---|---|
| `path` | Volg een verwijzing (of de omgekeerde verwijzing) en pas een filter toe op het doel. Maximaal 3 stappen. |
| `exists` | Het veld heeft een waarde. |
| `text` | Vrije tekst, alleen op velden die het profiel als doorzoekbaar markeert, en altijd binnen de rechten (zie [autorisatie](autorisatie.md)). |

Alleen velden die het profiel als `queryable` markeert, mogen in een filter of sortering staan. Zo weet de engine dat er een index voor is, en weet de beheerder wat er bevraagbaar is.

## Paden over verwijzingen

Dit is wat ZGW vandaag niet kan, en waar de G4 last van heeft: "zaken waar ik betrokkene ben" vraagt nu om een filter op een gerelateerde Rol, of om alles op te halen en in het portaal te filteren.

De engine houdt bij elke verwijzing in een projectie twee indexen bij: vooruit (`idx_field` op het verwijzende veld) en achteruit (`idx_ref`). Een pad wordt van de meest selectieve kant uitgevoerd. "Gesprekken bij zaken van zaaktype X" wordt dus: zaken met zaaktype X uit `idx_field`, daarna via `idx_ref` de gesprekken die ernaar verwijzen.

Een pad kan alleen over onderwerpen die in hetzelfde register staan. Verwijzingen naar buiten (een zaak in Open Zaak) kunnen meedoen als het register een **spiegel** van die bron bijhoudt: een profiel dat de events van die bron volgt en een kleine projectie bewaart (bijvoorbeeld zaak-URL, zaaktype en betrokkenen). Dat is precies het "per-domein register" uit ADR-0006 en de ingest uit ADR-0004.

## Representaties

Een representatie is wat je terugkrijgt: welke velden, en welke gerelateerde resources erin zijn opgenomen.

- `fields` kiest velden. Zonder `fields` krijg je de standaardvelden van het profiel.
- `include` neemt gerelateerde resources mee, met hun eigen `fields` en een maximum aantal per relatie. Dit lost het N+1-probleem op dat Rutger aankaartte bij de weergavegegevens.
- Veldmaskering geldt overal: wat je niet mag zien, staat er niet in, ook niet in een `include`.

### Benoemde representaties (views)

Een view is een query-template met parameters, geregistreerd door een beheerder of meegeleverd met een profiel:

```json
PUT /views/mijn-open-gesprekken
{
  "parameters": { "partij": { "type": "string", "format": "uri" } },
  "query": {
    "type": "nl.vng.gesprekken.gesprek",
    "filter": { "all": [
      { "exact": { "status": "open" } },
      { "path": ["deelnames"], "filter": { "exact": { "partij": "{partij}" } } }
    ]},
    "fields": ["onderwerp", "status", "laatsteBijdrage", "ongelezen"]
  },
  "policy": "gesprekken-inwoner"
}
```

Views zijn de domein-templates uit lab ADR-0003 ("WOZ, Parkeren, Belasting zijn query-templates bovenop deze grammatica"). Ze hebben drie voordelen:

1. **Cachebaar.** `GET /views/{naam}?...` mag als alle parameters niet-gevoelig zijn. Met een BSN of andere gevoelige waarde gebruik je `POST /views/{naam}`.
2. **Beleid per view.** Een view kan een eigen beleid hebben, zodat een beheerder kan zeggen "deze rol mag alleen deze views gebruiken" zonder vrije query's toe te staan.
3. **Stabiel contract.** Een portaal leunt op `mijn-open-gesprekken`, niet op de interne velden van het profiel.

## Live bevragen

Elke query-respons bevat een `feedCursor`: het volgnummer van het log op het moment van de snapshot. Een client die daarna `GET /events` opent met hetzelfde filter en `Last-Event-ID: <feedCursor>`, mist niets en krijgt niets dubbel. Dit is het patroon snapshot plus SSE uit lab ADR-0002, en het antwoord van dit ontwerp op de "live query" uit [lab issue #16](https://github.com/VNG-Realisatie/vng-api-lab/issues/16), zonder WebSockets.

Voor filters met paden is "dezelfde selectie volgen" lastiger: een gesprek kan in de selectie komen doordat de zaak van zaaktype verandert. De eerste versie stuurt daarom bij een live query met paden ook events van de onderwerpen waar het pad langs loopt, en laat de client opnieuw bevragen bij een `subject.matched` of `subject.unmatched`-melding. Later kan de engine dat zelf doen met bewaakte query's, zoals `WatchedQueries` in atomic-server.

## HTTP-methode

- `POST /query` met een JSON-body is de standaard. Persoonsgegevens staan dan niet in URL's en logs (eis A8).
- Zodra clients de HTTP-methode [`QUERY`](https://httpwg.org/http-extensions/draft-ietf-httpbis-safe-method-w-body.html) ondersteunen, accepteert hetzelfde pad dezelfde body met `QUERY`. Die is veilig en idempotent, en dus cachebaar.
- `GET` alleen voor views met niet-gevoelige parameters.

## Grenzen en kosten

Elke query krijgt vóór uitvoering een kostenschatting op basis van de indexen. Vaste grenzen in de eerste versie:

| Grens | Waarde |
|---|---|
| Diepte van filters | 3 niveaus |
| Elementen per `all` of `any` | 10 |
| Waarden in `in` | 50 |
| Stappen in een `path` | 3 |
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
