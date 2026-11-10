#!/usr/bin/env python3
"""Semi-realistische zaakevents voor een lokale OpenEvents (OPENEVENTS_DEV_AUTH=true).

Simuleert meldingen openbare ruimte (via een meldingen-API zoals Signalen),
omgevingsvergunningaanvragen (via het Omgevingsloket) en de gesprekken die
daarbij horen (profiel nl.vng.gesprekken, met aanleiding.zaak).

  examples/zaak-stream.py [BASE] [--backfill N] [--live] [--interval S] [--parallel N] [--count N]

  --backfill N   eerst N zaken met tijden in de afgelopen 30 dagen (chronologisch geschreven)
  --live         daarna doorlopend events, één per --interval seconden (standaard 2)
  --parallel N   aantal zaken dat in live-modus tegelijk loopt (standaard 6)
  --count N      stop na N live-events (standaard: nooit)

Zonder --backfill en --live doet het script: --backfill 25 --live.
Het profiel examples/profiles/zaken.json wordt eerst geregistreerd (als urn:admin).
"""
import argparse
import datetime as dt
import json
import os
import random
import sys
import time
import urllib.error
import urllib.request
import uuid

HERE = os.path.dirname(os.path.abspath(__file__))
ADMIN = "dev:urn:admin"
G = "nl.vng.gesprekken"
Z = "nl.demo.zaken"


# ---------- actoren ----------

def person(slug, **claims):
    uri = f"https://id.example.nl/{slug}"
    token = "dev:" + uri + "".join(f"|{k}={v}" for k, v in claims.items())
    return {"uri": uri, "token": token, "team": claims.get("team"), "slug": slug}


SIGNALEN = person("signalen", rol="systeem")
OLO = person("omgevingsloket", rol="systeem")
MEDEWERKERS = {
    "buitenruimte": [person("ahmed", rol="medewerker", team="buitenruimte"),
                     person("sanne", rol="medewerker", team="buitenruimte"),
                     person("dirk", rol="medewerker", team="buitenruimte")],
    "afval": [person("wouter", rol="medewerker", team="afval")],
    "handhaving": [person("bram", rol="medewerker", team="handhaving"),
                   person("noor", rol="medewerker", team="handhaving")],
    "vergunningen": [person("femke", rol="medewerker", team="vergunningen"),
                     person("rob", rol="medewerker", team="vergunningen")],
}
INWONERS = [person(s, rol="inwoner") for s in
            ["priya", "jan", "suzanne", "karel", "fatima", "henk", "lotte", "mohammed",
             "ingrid", "tim", "yara", "kees", "esther", "daan", "sevda", "pieter"]]
SOURCES = {
    "signalen": "https://signalen.demodam.nl",
    "omgevingsloket": "https://omgevingsloket.nl",
    "inwoner": "https://mijn.demodam.nl",
    "medewerker": "https://zaaksysteem.demodam.nl",
}


def source_for(actor):
    if actor is SIGNALEN:
        return SOURCES["signalen"]
    if actor is OLO:
        return SOURCES["omgevingsloket"]
    return SOURCES["medewerker"] if actor["team"] else SOURCES["inwoner"]


# ---------- inhoud ----------

STRATEN = [("Kerkstraat", "Centrum"), ("Molenweg", "Noord"), ("Lindenlaan", "Oost"),
           ("Havenkade", "Centrum"), ("Beatrixstraat", "West"), ("Populierenhof", "Zuid"),
           ("Dorpsstraat", "Oud-Zuid"), ("Schoolstraat", "Noord"), ("Merelstraat", "West"),
           ("Industrieweg", "Bedrijventerrein De Kamp"), ("Julianaplein", "Centrum"),
           ("Esdoornlaan", "Oost"), ("Weidepad", "Zuid"), ("Stationsweg", "Centrum")]


def locatie():
    straat, wijk = random.choice(STRATEN)
    return {"adres": f"{straat} {random.randint(1, 180)}, Demodam", "wijk": wijk,
            "lat": round(52.08 + random.random() * 0.04, 5),
            "lon": round(5.10 + random.random() * 0.05, 5)}


MELDINGEN = [
    dict(categorie="Wegen, verkeer, straatmeubilair", sub="Losse stoeptegel", team="buitenruimte",
         tekst=["Er ligt een stoeptegel los voor nummer {nr}, mijn buurvrouw is er bijna over gestruikeld.",
                "Meerdere tegels liggen scheef en los, gevaarlijk voor mensen met een rollator."],
         vraag="Kunt u ongeveer zeggen wanneer dit wordt gemaakt? Er lopen hier veel ouderen.",
         uitvoerder="Stratenmakersbedrijf Van Dijk"),
    dict(categorie="Wegen, verkeer, straatmeubilair", sub="Gat in de weg", team="buitenruimte",
         tekst=["Diep gat in het wegdek, fietsers moeten uitwijken naar de rijbaan.",
                "Na de vorst is er een flink gat in de weg ontstaan bij de kruising."],
         vraag="Is het mogelijk om alvast een bord of afzetting te plaatsen?",
         uitvoerder="Wegenonderhoud BV"),
    dict(categorie="Straatverlichting", sub="Lantaarnpaal kapot", team="buitenruimte",
         tekst=["De lantaarnpaal bij het speeltuintje brandt al een week niet.",
                "Lantaarnpaal knippert de hele nacht aan en uit."],
         vraag="Het is 's avonds echt donker in de straat, kan het voor het weekend?",
         uitvoerder="Netbeheerder Stedin"),
    dict(categorie="Groen en water", sub="Boom of tak gevallen", team="buitenruimte",
         tekst=["Na de storm ligt er een grote tak over het fietspad.",
                "Er hangt een afgebroken tak boven de stoep, die kan elk moment vallen."],
         vraag="Moet ik de tak zelf aan de kant leggen of komt er iemand?",
         uitvoerder="Hoveniersbedrijf De Groene Hand"),
    dict(categorie="Groen en water", sub="Verstopte put", team="buitenruimte",
         tekst=["Bij regen staat de hele straat blank, de putten zijn verstopt met bladeren."],
         vraag="Het water komt bijna tot aan onze voordeur, kan dit met spoed?",
         uitvoerder="Rioolservice West"),
    dict(categorie="Afval", sub="Grofvuil naast container", team="afval",
         tekst=["Er staat een bank en een matras naast de ondergrondse container.",
                "Iemand heeft vuilniszakken naast de container gezet, de meeuwen trekken ze open."],
         vraag="Wordt de vervuiler hier ook op aangesproken?", uitvoerder="Afvalinzameling Demodam"),
    dict(categorie="Afval", sub="Container vol of kapot", team="afval",
         tekst=["De papiercontainer zit vol en de klep klemt.", "De pasjeslezer van de restafvalcontainer werkt niet."],
         vraag="Waar kan ik mijn afval in de tussentijd kwijt?", uitvoerder="Afvalinzameling Demodam"),
    dict(categorie="Overlast", sub="Geluidsoverlast horeca", team="handhaving",
         tekst=["Het café op de hoek draait na 01:00 nog harde muziek met de deuren open.",
                "Elke vrijdagnacht veel lawaai van bezoekers die buiten blijven hangen."],
         vraag="Dit gebeurt elke week. Wat kan de gemeente hier structureel aan doen?",
         uitvoerder="Boa-team Centrum"),
    dict(categorie="Overlast", sub="Fout geparkeerde fietsen", team="handhaving",
         tekst=["Bij de ingang van het station staan tientallen fietsen op de geleidelijn voor blinden."],
         vraag="Worden de fietsen weggehaald of krijgen mensen eerst een waarschuwing?",
         uitvoerder="Fietsdepot Demodam"),
]

VERGUNNINGEN = [
    dict(activiteit="Bouwen dakkapel", omschrijving="Plaatsen van een dakkapel aan de achterzijde, breedte 3,2 meter.",
         bijlagen=["bouwtekening-dakkapel.pdf", "constructieberekening.pdf", "foto-bestaande-situatie.jpg"],
         adviseur="Welstandscommissie", vraag="Past de dakkapel binnen de welstandsnota?",
         aanvulling=["constructieberekening met handtekening constructeur"], kosten=(8000, 18000)),
    dict(activiteit="Kappen boom", omschrijving="Kappen van een eik in de voortuin vanwege wortelopdruk en schade aan de fundering.",
         bijlagen=["foto-boom.jpg", "situatieschets.pdf"],
         adviseur="Bomenconsulent", vraag="Is de boom waardevol en is herplant mogelijk?",
         aanvulling=["rapport boomveiligheidscontrole"], kosten=(0, 0)),
    dict(activiteit="Aanbouw woning", omschrijving="Uitbreiding van de woonkamer met een aanbouw van 4 bij 3 meter aan de achtergevel.",
         bijlagen=["plattegrond.pdf", "doorsnede.pdf", "gevelaanzichten.pdf", "BENG-berekening.pdf"],
         adviseur="Welstandscommissie", vraag="Graag advies over materiaalgebruik en kleur.",
         aanvulling=["daglichtberekening", "toelichting afvoer hemelwater"], kosten=(25000, 60000)),
    dict(activiteit="Uitweg aanleggen", omschrijving="Aanleggen van een inrit van 3 meter breed naar de oprit.",
         bijlagen=["situatietekening-uitweg.pdf"],
         adviseur="Verkeerskundige", vraag="Is de uitweg verkeersveilig gezien de nabijgelegen school?",
         aanvulling=["maatvoering ten opzichte van de lantaarnpaal"], kosten=(2000, 5000)),
    dict(activiteit="Verbouwen rijksmonument", omschrijving="Vervangen van kozijnen in de voorgevel door houten kozijnen met isolatieglas.",
         bijlagen=["bouwhistorische-verkenning.pdf", "detailtekeningen-kozijnen.pdf", "kleurenschema.pdf"],
         adviseur="Erfgoedcommissie", vraag="Graag advies over de profielen en het glas.",
         aanvulling=["detail aansluiting kozijn op metselwerk"], kosten=(15000, 40000), uitgebreid=True),
    dict(activiteit="Zonnepanelen op monument", omschrijving="Plaatsen van 14 zonnepanelen op het achterdakvlak.",
         bijlagen=["legplan.pdf", "foto-dakvlak.jpg"],
         adviseur="Erfgoedcommissie", vraag="Zijn de panelen vanaf de openbare weg zichtbaar?",
         aanvulling=["zichtlijnenstudie vanaf de straat"], kosten=(6000, 12000)),
]

AFHANDELING = {
    "opgelost": ["De tegels zijn opnieuw gelegd.", "Het probleem is verholpen door {uitvoerder}.",
                 "Opgeruimd en de omgeving is schoongemaakt."],
    "doorgezet naar derde": ["Doorgezet naar {uitvoerder}, die neemt het verder op."],
    "dubbele melding": ["Er was al een melding over deze locatie, die is inmiddels afgehandeld."],
}


# ---------- scenario's ----------
# Een scenario is een lijst stappen (minuten na start, actor, type, subject, data).

class Seq:
    def __init__(self):
        self.t = 0
        self.steps = []

    def at(self, minutes_later, actor, type_, subject, data):
        self.t += minutes_later
        self.steps.append((self.t, actor, type_, subject, data))


def nummer(prefix):
    return f"{prefix}-2026-{random.randint(100000, 999999)}"


def gesprek_door_inwoner(s, inwoner, zaak_uri, team, medewerker, onderwerp, vraag, antwoord, na=30):
    g = f"urn:uuid:{uuid.uuid4()}"
    s.at(na, inwoner, f"{G}.gesprek.gestart", g, {
        "onderwerp": onderwerp, "team": team, "aanleiding": {"zaak": zaak_uri},
        "initiatiefnemer": {"partij": inwoner["uri"], "rol": "inwoner"}})
    s.at(1, inwoner, f"{G}.bijdrage.toegevoegd", g, {"tekst": vraag})
    s.at(random.randint(60, 600), medewerker, f"{G}.bijdrage.toegevoegd", g, {"tekst": antwoord, "automatisch": False})
    return g


def melding_scenario():
    m = random.choice(MELDINGEN)
    inwoner = random.choice(INWONERS)
    mw = random.choice(MEDEWERKERS[m["team"]])
    nr = nummer("MOR")
    zaak = f"https://zaken.demodam.nl/meldingen/{nr}"
    loc = locatie()
    s = Seq()
    s.at(0, SIGNALEN, f"{Z}.melding.gedaan", zaak, {
        "zaaknummer": nr, "categorie": m["categorie"], "subcategorie": m["sub"],
        "omschrijving": random.choice(m["tekst"]).format(nr=random.randint(1, 120)),
        "melder": inwoner["uri"], "kanaal": random.choice(["app", "app", "website", "telefoon", "e-mail"]),
        "team": m["team"], "prioriteit": random.choice(["normaal", "normaal", "laag", "hoog"]),
        "locatie": loc, "fotos": random.choice([0, 1, 1, 2, 3])})
    s.at(random.randint(20, 900), mw, f"{Z}.melding.toegewezen", zaak, {"behandelaar": mw["uri"], "team": m["team"]})
    gesprek = None
    r = random.random()
    if r < 0.35:
        gesprek = gesprek_door_inwoner(
            s, inwoner, zaak, m["team"], mw, f"Vraag over melding {nr}", m["vraag"],
            random.choice(["We hebben het ingepland, de verwachting is binnen 5 werkdagen.",
                           "Goed dat u het meldt. Een collega gaat vandaag of morgen kijken.",
                           "We hebben een tijdelijke afzetting geplaatst, herstel volgt deze week."]))
    elif r < 0.6:
        g = f"urn:uuid:{uuid.uuid4()}"
        gesprek = g
        s.at(random.randint(10, 240), mw, f"{G}.gesprek.gestart", g, {
            "onderwerp": f"Aanvullende vraag bij melding {nr}", "team": m["team"],
            "aanleiding": {"zaak": zaak}, "initiatiefnemer": {"partij": mw["uri"], "rol": "behandelaar"}})
        s.at(1, mw, f"{G}.deelname.toegevoegd", g, {"partij": inwoner["uri"], "rol": "inwoner"})
        s.at(1, mw, f"{G}.bijdrage.toegevoegd", g, {"tekst": random.choice([
            f"Wij kunnen de locatie bij {loc['adres'].split(',')[0]} niet goed vinden. Kunt u een foto sturen?",
            "Is het nog steeds zo, of is het inmiddels opgelost?",
            "Klopt het dat het om de kant van de school gaat?"])})
        s.at(random.randint(20, 400), inwoner, f"{G}.bijdrage.toegevoegd", g, {"tekst": random.choice([
            "Ja, het is er nog steeds. Ik heb net een foto bij de melding gezet.",
            "Aan de kant van de school inderdaad, vlak bij het zebrapad.",
            "Het is zelfs erger geworden sinds gisteren."])})
        s.at(2, inwoner, f"{Z}.melding.aangevuld", zaak, {"tekst": "Extra foto toegevoegd via het gesprek.", "fotos": 1})
    elif r < 0.75:
        s.at(random.randint(60, 1440), inwoner, f"{Z}.melding.aangevuld", zaak, {
            "tekst": random.choice(["Het wordt steeds erger.", "Vanochtend is er bijna een fietser gevallen.",
                                    "Inmiddels hebben meer buren hier last van."])})
    if random.random() < 0.6:
        datum = (dt.date.today() + dt.timedelta(days=random.randint(1, 10))).isoformat()
        s.at(random.randint(120, 2880), mw, f"{Z}.melding.ingepland", zaak, {
            "datum": datum, "uitvoerder": m["uitvoerder"], "toelichting": "Werkzaamheden ingepland."})
    resultaat = random.choices(["opgelost", "doorgezet naar derde", "dubbele melding"], [8, 2, 1])[0]
    s.at(random.randint(240, 4320), mw, f"{Z}.melding.afgehandeld", zaak, {
        "resultaat": resultaat,
        "toelichting": random.choice(AFHANDELING[resultaat]).format(uitvoerder=m["uitvoerder"])})
    if gesprek:
        s.at(random.randint(5, 120), mw, f"{G}.bijdrage.toegevoegd", gesprek, {
            "tekst": "De melding is afgehandeld. Laat het ons weten als het probleem terugkomt."})
        s.at(1, mw, f"{G}.gesprek.afgesloten", gesprek, {"reden": "Melding afgehandeld"})
    if random.random() < 0.4:
        s.at(random.randint(60, 2880), SIGNALEN, f"{Z}.melding.tevredenheid-gegeven", zaak, {
            "score": random.choices([5, 4, 3, 2, 1], [5, 4, 2, 1, 1])[0],
            "opmerking": random.choice(["Snel opgelost, dank!", "Duurde wat lang.", "Prima geholpen.", ""])})
    return s.steps


def vergunning_scenario():
    v = random.choice(VERGUNNINGEN)
    aanvrager = random.choice(INWONERS)
    mw = random.choice(MEDEWERKERS["vergunningen"])
    nr = nummer("OV")
    zaak = f"https://zaken.demodam.nl/vergunningen/{nr}"
    uitgebreid = v.get("uitgebreid", False)
    beslis = (dt.date.today() + dt.timedelta(weeks=26 if uitgebreid else 8)).isoformat()
    s = Seq()
    data = {
        "zaaknummer": nr, "olonummer": str(random.randint(9000000, 9999999)), "activiteit": v["activiteit"],
        "omschrijving": v["omschrijving"], "aanvrager": aanvrager["uri"],
        "procedure": "uitgebreid" if uitgebreid else "regulier", "team": "vergunningen",
        "locatie": {**{k: x for k, x in locatie().items() if k in ("adres", "wijk")},
                    "kadastraal": f"DMD00 K {random.randint(1000, 9999)}"},
        "bijlagen": v["bijlagen"], "uiterlijkeBeslisdatum": beslis}
    lo, hi = v["kosten"]
    if hi:
        data["bouwkosten"] = random.randint(lo, hi) // 100 * 100
    if random.random() < 0.3:
        data["gemachtigde"] = "https://id.example.nl/architectenbureau-de-boog"
    s.at(0, OLO, f"{Z}.vergunningaanvraag.ingediend", zaak, data)
    s.at(random.randint(600, 4320), mw, f"{Z}.vergunningaanvraag.ontvankelijk-verklaard", zaak, {
        "behandelaar": mw["uri"], "toelichting": "Aanvraag is compleet genoeg om in behandeling te nemen."})
    if random.random() < 0.45:
        termijn = (dt.date.today() + dt.timedelta(weeks=2)).isoformat()
        s.at(random.randint(600, 2880), mw, f"{Z}.vergunningaanvraag.aanvulling-gevraagd", zaak, {
            "gevraagd": v["aanvulling"], "termijn": termijn,
            "toelichting": "De beslistermijn is opgeschort tot de stukken binnen zijn."})
        g = f"urn:uuid:{uuid.uuid4()}"
        s.at(5, mw, f"{G}.gesprek.gestart", g, {
            "onderwerp": f"Aanvulling nodig voor {nr} ({v['activiteit'].lower()})", "team": "vergunningen",
            "aanleiding": {"zaak": zaak}, "initiatiefnemer": {"partij": mw["uri"], "rol": "behandelaar"}})
        s.at(1, mw, f"{G}.deelname.toegevoegd", g, {"partij": aanvrager["uri"], "rol": "inwoner"})
        s.at(1, mw, f"{G}.bijdrage.toegevoegd", g, {
            "tekst": f"Voor uw aanvraag missen we nog: {', '.join(v['aanvulling'])}. U kunt dit aanleveren tot {termijn}."})
        s.at(random.randint(60, 1440), aanvrager, f"{G}.bijdrage.toegevoegd", g, {"tekst": random.choice([
            "Dank voor het bericht. Mijn aannemer stuurt het deze week op.",
            "Moet dat document ook ondertekend zijn?",
            "Ik begrijp niet goed wat u precies nodig heeft, kunt u bellen?"])})
        s.at(random.randint(30, 480), mw, f"{G}.bijdrage.toegevoegd", g, {"tekst": random.choice([
            "Ja, graag ondertekend door de constructeur.", "Prima, u kunt het uploaden via het Omgevingsloket.",
            "Ik bel u morgenochtend."])})
        s.at(random.randint(1440, 7200), aanvrager, f"{Z}.vergunningaanvraag.aanvulling-aangeleverd", zaak, {
            "bijlagen": [a.replace(" ", "-") + ".pdf" for a in v["aanvulling"]], "toelichting": "Zie bijlage."})
        s.at(random.randint(10, 600), mw, f"{G}.gesprek.afgesloten", g, {"reden": "Aanvulling ontvangen"})
    if random.random() < 0.08:
        s.at(random.randint(1440, 10080), OLO, f"{Z}.vergunningaanvraag.ingetrokken", zaak, {
            "reden": "Aanvrager heeft de aanvraag ingetrokken via het Omgevingsloket."})
        return s.steps
    if random.random() < 0.6 or uitgebreid:
        s.at(random.randint(600, 2880), mw, f"{Z}.vergunningaanvraag.advies-gevraagd", zaak, {
            "adviseur": v["adviseur"], "vraag": v["vraag"]})
        s.at(random.randint(4320, 20160), mw, f"{Z}.vergunningaanvraag.advies-uitgebracht", zaak, {
            "adviseur": v["adviseur"],
            "advies": random.choices(["positief", "positief met voorwaarden", "negatief"], [5, 3, 1])[0],
            "toelichting": random.choice(["Past binnen het beleid.", "Akkoord, mits de kleur wordt aangepast.",
                                          "Niet passend in het straatbeeld."])})
    besluit = random.choices(["verleend", "verleend met voorschriften", "geweigerd"], [6, 3, 1])[0]
    bd = {"besluit": besluit, "bezwaartermijnTot": (dt.date.today() + dt.timedelta(weeks=6)).isoformat()}
    if besluit == "verleend met voorschriften":
        bd["voorschriften"] = random.sample(["Start bouw melden 2 dagen vooraf", "Herplantplicht binnen 12 maanden",
                                             "Kleur kozijnen RAL 9001", "Geen werkzaamheden in het broedseizoen"], 2)
    s.at(random.randint(1440, 10080), mw, f"{Z}.vergunningaanvraag.besluit-genomen", zaak, bd)
    return s.steps


def scenario():
    return melding_scenario() if random.random() < 0.65 else vergunning_scenario()


# ---------- HTTP ----------

def request(base, method, path, token, body):
    req = urllib.request.Request(base + path, method=method, data=json.dumps(body).encode(),
                                 headers={"authorization": f"Bearer {token}", "content-type": "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=10) as r:
            return r.status, r.read()
    except urllib.error.HTTPError as e:
        return e.code, e.read()


def post_event(base, actor, type_, subject, data, when=None):
    ev = {"specversion": "1.0", "id": str(uuid.uuid4()), "source": source_for(actor),
          "type": type_, "subject": subject, "datacontenttype": "application/json", "data": data}
    if when:
        ev["time"] = when.isoformat().replace("+00:00", "Z")
    code, body = request(base, "POST", "/events", actor["token"], ev)
    short = type_.replace(Z + ".", "").replace(G + ".", "gesprek:")
    if code >= 300:
        print(f"  ! {code} {short} {subject}: {body.decode()[:300]}", file=sys.stderr)
        return False
    print(f"  {when.strftime('%Y-%m-%d %H:%M') if when else 'nu':>16}  {actor['slug']:<15} {short:<45} {subject}")
    return True


def install_profile(base):
    with open(os.path.join(HERE, "profiles", "zaken.json")) as f:
        profile = json.load(f)
    code, body = request(base, "PUT", f"/profiles/{profile['id']}", ADMIN, profile)
    if code >= 300:
        sys.exit(f"profiel registreren mislukt ({code}): {body.decode()[:500]}")
    print(f"profiel {profile['id']} geregistreerd")


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("base", nargs="?", default="http://localhost:8080")
    ap.add_argument("--backfill", type=int)
    ap.add_argument("--live", action="store_true")
    ap.add_argument("--interval", type=float, default=2.0)
    ap.add_argument("--parallel", type=int, default=6)
    ap.add_argument("--count", type=int)
    a = ap.parse_args()
    if a.backfill is None and not a.live:
        a.backfill, a.live = 25, True
    base = a.base.rstrip("/")
    install_profile(base)

    if a.backfill:
        now = dt.datetime.now(dt.timezone.utc)
        planned = []
        for _ in range(a.backfill):
            start = now - dt.timedelta(minutes=random.randint(60, 30 * 24 * 60))
            for minutes, actor, type_, subject, data in scenario():
                when = start + dt.timedelta(minutes=minutes)
                if when < now:  # wat in de toekomst ligt, gebeurt (nog) niet: die zaken blijven open
                    planned.append((when, actor, type_, subject, data))
        planned.sort(key=lambda x: x[0])
        print(f"backfill: {a.backfill} zaken, {len(planned)} events")
        ok = sum(post_event(base, actor, t, s, d, when) for when, actor, t, s, d in planned)
        print(f"backfill klaar: {ok}/{len(planned)} geschreven")

    if a.live:
        print(f"live: elke {a.interval}s een event, {a.parallel} zaken tegelijk (Ctrl+C om te stoppen)")
        active, n = [], 0
        try:
            while a.count is None or n < a.count:
                while len(active) < a.parallel:
                    active.append(list(scenario()))
                steps = random.choice(active)
                _, actor, type_, subject, data = steps.pop(0)
                post_event(base, actor, type_, subject, data)
                if not steps:
                    active.remove(steps)
                n += 1
                time.sleep(a.interval)
        except KeyboardInterrupt:
            print(f"\ngestopt na {n} live-events")


if __name__ == "__main__":
    main()
