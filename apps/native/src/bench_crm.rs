//! The bench's synthetic CRM (`--bench-shot crm…`): 1,240 companies, about 2,100 contacts (some do-not-contact), 420
//! deals across the stages, 600 timeline entries from the GTM crew, a change log per record, two webhooks with their
//! deliveries, and the GTM crew's templates and bundle. The fake API answers the CRM's reads with the same filters,
//! sorts and paging the real one applies, and the few writes the screenshots need (a dry-run import, adding and
//! testing a webhook, moving a deal, hiring the crew).

use std::collections::HashMap;

use chrono::{DateTime, Duration, Utc};
use familiar_client::{
    ActivityKind, Avatar, Bot, BotEngine, BotSetup, BotStatus, CrmActivity, CrmChange, CrmCompany,
    CrmContact, CrmDeal, CrmWebhook, CrmWebhookDelivery, DealStage, Hired, PipelineDeal, PipelineStage, RunKind, Schedule,
    SetupLogin, Template, TemplateBundle, TemplateLogin, TemplateQuestion, TemplateSchedule,
};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::crm_model::STAGES;

const PREFIX: [&str; 40] = [
    "Acme", "Northwind", "Globex", "Initech", "Umbrella", "Hooli", "Vandelay", "Stark", "Wayne", "Cyberdyne", "Tyrell",
    "Soylent", "Wonka", "Aperture", "Monarch", "Oscorp", "Massive", "Sirius", "Gekko", "Bluth", "Dunder", "Prestige",
    "Sterling", "Pendant", "Kramerica", "Nakatomi", "Zorg", "Duff", "Vehement", "Spacely", "Cogswell", "Rekall",
    "Ollivander", "Ghostwood", "Bushwood", "Burleigh", "Brightline", "Brawndo", "Hanso", "Ingen",
];
const SUFFIX: [&str; 31] = [
    "Robotics", "Labs", "Analytics", "Cloud", "Health", "Logistics", "Systems", "AI", "Security", "Payments", "Data",
    "Networks", "Software", "Energy", "Bio", "Mobility", "Studio", "Capital", "Foods", "Devices", "Insights", "Ops",
    "Works", "Media", "Learning", "Freight", "Retail", "Space", "Climate", "Legal", "Hire",
];
const INDUSTRY: [&str; 8] = ["B2B SaaS", "Fintech", "Developer tools", "Healthtech", "Logistics", "Security", "Climate", "Marketplaces"];
const SIZE: [&str; 5] = ["11-50", "51-200", "51-200", "201-500", "501-1000"];
const CITY: [&str; 9] = ["Berlin", "London", "New York", "Austin", "Amsterdam", "Toronto", "Paris", "Lisbon", "San Francisco"];
const TAGS: [&str; 10] = ["saas", "series-a", "series-b", "eu", "us", "devtools", "platform-team", "hiring", "fintech", "warm-intro"];
const FIRST: [&str; 30] = [
    "Sam", "Priya", "Jonas", "Maya", "Leo", "Amara", "Noah", "Ines", "Kenji", "Zoe", "Omar", "Lena", "Diego", "Hana",
    "Felix", "Nora", "Ravi", "Elsa", "Tom", "Yara", "Ben", "Chloe", "Ivan", "Mei", "Luca", "Sofia", "Arjun", "Grace",
    "Max", "Ada",
];
const LAST: [&str; 30] = [
    "Rivera", "Patel", "Becker", "Okafor", "Nguyen", "Schmidt", "Kowalski", "Haddad", "Tanaka", "Moreau", "Silva",
    "Larsen", "Cohen", "Ibrahim", "Rossi", "Fischer", "Khan", "Novak", "Ward", "Lindqvist", "Costa", "Murphy", "Sato",
    "Weber", "Diaz", "Mendes", "Hughes", "Kaur", "Brandt", "Vogel",
];
const TITLES: [&str; 10] = [
    "Head of Platform", "VP Engineering", "CTO", "Staff SRE", "Director of Infrastructure", "Engineering Manager",
    "Founder & CEO", "Head of Security", "Platform Lead", "Principal Engineer",
];
const NEXT: [&str; 7] = [
    "Send the case study from the Hooli rollout",
    "Book a demo with their platform team",
    "Answer the pricing question from Tuesday's call",
    "Wait for their reply to the first email",
    "Share the security questionnaire",
    "Intro call with the VP Engineering",
    "Follow up after their launch week",
];

pub fn cid(kind: u128, n: usize) -> Uuid {
    Uuid::from_u128(0xc4a0_0000_0000_4000_8000_0000_0000_0000 | (kind << 32) | n as u128)
}

/// The deal stage of the `i`th deal: most are early, a few are won or lost.
fn stage_of(i: usize) -> DealStage {
    const SPREAD: [DealStage; 20] = [
        DealStage::New, DealStage::New, DealStage::New, DealStage::New, DealStage::New, DealStage::Researching,
        DealStage::Researching, DealStage::Contacted, DealStage::Contacted, DealStage::Contacted, DealStage::Contacted,
        DealStage::Replied, DealStage::Replied, DealStage::Meeting, DealStage::Meeting, DealStage::Proposal,
        DealStage::Won, DealStage::Lost, DealStage::Lost, DealStage::Contacted,
    ];
    SPREAD[(i * 7) % SPREAD.len()]
}

/// A hired crew: its teammates and their schedules (for the fixture).
pub type Hire = (Vec<Bot>, Vec<Schedule>);

/// The crew's teammates as the bench shows them (avatars from their templates).
pub struct CrewIds {
    pub lead: Uuid,
    pub drafter: Uuid,
    pub tracker: Uuid,
}

pub struct Crm {
    now: DateTime<Utc>,
    crew: CrewIds,
    pub companies: Vec<CrmCompany>,
    pub contacts: Vec<CrmContact>,
    pub deals: Vec<CrmDeal>,
    pub activities: Vec<CrmActivity>,
    pub webhooks: Vec<CrmWebhook>,
    deliveries: Vec<CrmWebhookDelivery>,
    pub templates: Vec<Template>,
    pub bundle: TemplateBundle,
    /// The approval a timeline entry waits on (a draft in Needs you).
    pub waiting_draft: Uuid,
}

fn avatar(shape: u32, color: &str, eyes: u32, mouth: u32, accessory: &str) -> Avatar {
    Avatar { shape: Some(shape), color: Some(color.into()), eyes: Some(eyes), mouth: Some(mouth), accessory: Some(accessory.into()) }
}

/// The GTM crew's templates, as `GET /api/templates` lists them.
pub fn crew_templates() -> Vec<Template> {
    let t = |id: &str, name: &str, category: &str, summary: &str, av: Avatar, sched: &[(&str, &str)], logins: &[(&str, &str)], conn: &[&str]| Template {
        id: id.into(),
        name: name.into(),
        category: category.into(),
        summary: summary.into(),
        avatar: Some(av),
        model: "sonnet".into(),
        schedules: sched.iter().map(|(l, c)| TemplateSchedule { label: (*l).into(), cron: (*c).into(), prompt: String::new() }).collect(),
        logins: logins.iter().map(|(s, u)| TemplateLogin { site: (*s).into(), url: (*u).into() }).collect(),
        connectors: conn.iter().map(|c| (*c).to_owned()).collect(),
        ..Default::default()
    };
    vec![
        t(
            "lead-researcher",
            "Lead researcher",
            "Sales",
            "Finds companies that match your ideal customer through public signals (funding, hiring, launches) and adds them to your CRM with sources and a fit score.",
            avatar(2, "#4fa9cf", 3, 0, "glasses"),
            &[("Find new leads", "0 8 * * 2,4"), ("Weekly lead summary", "0 16 * * 5")],
            &[],
            &["brave-search", "fetch"],
        ),
        t(
            "outbound-drafter",
            "Outbound drafter",
            "Sales",
            "Writes personalised first emails and LinkedIn notes for the new deals in your CRM, grounded in why each one fits. Sends only what you approve.",
            avatar(1, "#7285d5", 0, 3, "hat"),
            &[("Draft outreach for new leads", "30 9 * * 1-5")],
            &[("LinkedIn", "https://www.linkedin.com/login")],
            &["google-workspace"],
        ),
        t(
            "reply-follow-up-tracker",
            "Reply & follow-up tracker",
            "Sales",
            "Twice a day, matches replies to your CRM, keeps every deal's stage true, honours opt-outs at once and drafts the few follow-ups worth sending.",
            avatar(3, "#eda84b", 2, 2, "antenna"),
            &[("Check replies", "0 9,15 * * 1-5")],
            &[("LinkedIn", "https://www.linkedin.com/login")],
            &["google-workspace"],
        ),
        t(
            "weekly-metrics-reporter",
            "Weekly metrics reporter",
            "Growth & marketing",
            "Collects your key numbers from your dashboards every Monday and writes a two-minute report on what moved and why.",
            avatar(2, "#7285d5", 4, 0, "glasses"),
            &[("Monday report", "0 7 * * 1")],
            &[("Stripe", "https://dashboard.stripe.com/login"), ("PostHog", "https://us.posthog.com/login")],
            &["slack", "postgres"],
        ),
        t(
            "inbox-assistant",
            "Inbox assistant",
            "Personal",
            "Triages your Gmail twice a day, tells you what needs you, and drafts replies for you to send. Never sends or deletes.",
            avatar(1, "#4fb98a", 0, 1, "none"),
            &[("Morning triage", "0 8 * * 1-5"), ("Afternoon sweep", "0 14 * * 1-5")],
            &[],
            &["google-workspace"],
        ),
    ]
}

pub fn crew_bundle() -> TemplateBundle {
    let q = |key: &str, label: &str, placeholder: &str, multiline: bool| TemplateQuestion {
        key: key.into(),
        label: label.into(),
        placeholder: placeholder.into(),
        multiline,
    };
    TemplateBundle {
        id: "gtm-crew".into(),
        name: "GTM crew".into(),
        summary: "A small sales team that keeps its work in your CRM: one finds leads, one writes first emails, one tracks replies and follow-ups, one reports the pipeline weekly, and one triages your inbox. Nothing goes out without your OK, and every schedule starts off.".into(),
        templates: crew_templates().into_iter().map(|t| t.id).collect(),
        questions: vec![
            q("product", "What do you sell?", "an incident-response tool for platform teams", true),
            q("icp", "Your ideal customer", "B2B SaaS, 20 to 200 people, Series A or later, US or EU, with a platform or SRE team", true),
            q("offer", "What's the ask?", "a 20-minute call, or a free two-week pilot", false),
            q("sender", "Who is it from?", "Sam, co-founder", false),
            q("voice", "How should it sound?", "direct and friendly, no flattery, under 120 words", true),
            q("follow_up_days", "Days of silence before a follow-up", "4", false),
        ],
    }
}

/// The crew's first three teammates as bots of the fixture (their rows show who added what).
pub fn crew_bots(now: DateTime<Utc>, ids: &CrewIds) -> Vec<Bot> {
    let tpl = crew_templates();
    let mk = |id: Uuid, t: &Template, status: BotStatus, last: i64| Bot {
        id,
        slug: t.id.clone(),
        name: t.name.clone(),
        persona: Some(t.summary.clone()),
        model: "sonnet".into(),
        engine: BotEngine::Claude,
        avatar: t.avatar.clone(),
        status: Some(status),
        last_run_at: Some(now - Duration::minutes(last)),
        created_at: now - Duration::days(9),
        ..Default::default()
    };
    vec![
        mk(ids.lead, &tpl[0], BotStatus::Running, 2),
        mk(ids.drafter, &tpl[1], BotStatus::Idle, 35),
        mk(ids.tracker, &tpl[2], BotStatus::Idle, 180),
    ]
}

impl Crm {
    pub fn new(now: DateTime<Utc>, crew: CrewIds, empty: bool) -> Self {
        let mut this = Self {
            now,
            crew,
            companies: Vec::new(),
            contacts: Vec::new(),
            deals: Vec::new(),
            activities: Vec::new(),
            webhooks: Vec::new(),
            deliveries: Vec::new(),
            templates: crew_templates(),
            bundle: crew_bundle(),
            waiting_draft: cid(9, 1),
        };
        if !empty {
            this.fill();
        }
        this
    }

    fn fill(&mut self) {
        let now = self.now;
        let ago = |m: i64| now - Duration::minutes(m);
        for i in 0..PREFIX.len() * SUFFIX.len() {
            let name = format!("{} {}", PREFIX[i % PREFIX.len()], SUFFIX[(i / PREFIX.len()) % SUFFIX.len()]);
            let slug = name.to_lowercase().replace(' ', "");
            let domain = format!("{slug}.{}", if i % 3 == 0 { "io" } else { "com" });
            let fit = (i % 9 != 4).then_some(((i * 37 + 11) % 71 + 29) as i32);
            let tags: Vec<String> = (0..(i % 4)).map(|k| TAGS[(i * 3 + k * 5) % TAGS.len()].to_owned()).collect();
            let by_bot = i % 5 != 3;
            self.companies.push(CrmCompany {
                id: cid(1, i),
                name: name.clone(),
                domain: Some(domain.clone()),
                website: Some(format!("https://{domain}")),
                industry: Some(INDUSTRY[i % INDUSTRY.len()].into()),
                size: Some(SIZE[i % SIZE.len()].into()),
                location: Some(CITY[i % CITY.len()].into()),
                description: Some(format!(
                    "{name} builds {} for {} teams. Raised a Series {} last spring; their engineering blog talks about on-call load.",
                    SUFFIX[(i / PREFIX.len()) % SUFFIX.len()].to_lowercase(),
                    INDUSTRY[(i + 3) % INDUSTRY.len()].to_lowercase(),
                    ["A", "B", "C"][i % 3]
                )),
                fit_score: fit,
                fit_reason: fit.map(|_| {
                    format!("Hiring {} SREs and their status page shows {} incidents last quarter: on-call pain is real.", 2 + i % 4, 3 + i % 6)
                }),
                tags,
                source_urls: vec![format!("https://{domain}/careers"), format!("https://news.example.com/{slug}-raises")],
                custom: json!({}),
                created_by_bot: by_bot.then_some(self.crew.lead),
                created_at: ago(60 * 24 * 9 - i as i64 * 9),
                updated_at: ago(7 + i as i64 * 13),
            });
        }
        let mut n = 0;
        for (ci, co) in self.companies.iter().enumerate().take(1050) {
            for k in 0..(1 + (ci % 3 == 0) as usize + (ci % 7 == 0) as usize) {
                let (f, l) = (FIRST[(n * 7 + k) % FIRST.len()], LAST[(n * 11 + ci) % LAST.len()]);
                let dnc = n % 37 == 5;
                let domain = co.domain.clone().unwrap_or_default();
                self.contacts.push(CrmContact {
                    id: cid(2, n),
                    company_id: Some(co.id),
                    company_name: Some(co.name.clone()),
                    company_domain: co.domain.clone(),
                    name: format!("{f} {l}"),
                    title: Some(TITLES[(n + ci) % TITLES.len()].into()),
                    email: Some(format!("{}.{}@{domain}", f.to_lowercase(), l.to_lowercase())),
                    linkedin_url: Some(format!("https://www.linkedin.com/in/{}-{}", f.to_lowercase(), l.to_lowercase())),
                    notes: (n % 4 == 0).then(|| "Spoke at a platform meetup about incident reviews. Prefers async.".into()),
                    tags: if n % 6 == 0 { vec!["champion".into()] } else { Vec::new() },
                    source_urls: vec![format!("https://{domain}/team")],
                    custom: json!({}),
                    do_not_contact: dnc,
                    dnc_reason: dnc.then(|| "Replied “please stop emailing me”".into()),
                    dnc_at: dnc.then(|| ago(60 * 30 + n as i64)),
                    created_by_bot: (n % 5 != 1).then_some(self.crew.lead),
                    created_at: co.created_at,
                    updated_at: ago(11 + n as i64 * 17),
                    ..Default::default()
                });
                n += 1;
            }
        }
        for i in 0..420 {
            let co = &self.companies[i * 2];
            let contact = self.contacts.iter().find(|c| c.company_id == Some(co.id));
            let stage = stage_of(i);
            let short = co.name.split(' ').next().unwrap_or("").to_owned();
            let open = !matches!(stage, DealStage::Won | DealStage::Lost);
            self.deals.push(CrmDeal {
                id: cid(3, i),
                company_id: co.id,
                contact_id: contact.map(|c| c.id),
                company_name: Some(co.name.clone()),
                company_domain: co.domain.clone(),
                contact_name: contact.map(|c| c.name.clone()),
                contact_email: contact.and_then(|c| c.email.clone()),
                contact_do_not_contact: contact.is_some_and(|c| c.do_not_contact),
                title: match i % 4 {
                    0 => format!("{short} pilot"),
                    1 => format!("{short}: incident response rollout"),
                    2 => format!("{short} annual plan"),
                    _ => format!("{short}: on-call for the platform team"),
                },
                stage,
                stage_changed_at: ago(30 + i as i64 * 41),
                value_cents: (i % 6 != 2).then_some(((i * 7919) % 70 + 5) as i64 * 100_000),
                currency: "USD".into(),
                next_step: open.then(|| NEXT[i % NEXT.len()].into()),
                next_step_at: open.then(|| now + Duration::days((i as i64 % 15) - 4)),
                created_by_bot: (i % 4 != 0).then_some(self.crew.lead),
                created_at: ago(60 * 24 * 8 - i as i64 * 11),
                updated_at: ago(5 + i as i64 * 23),
            });
        }
        // The timeline: newest first, the crew at work.
        for i in 0..600 {
            let d = &self.deals[(i * 3) % self.deals.len()];
            let contact = d.contact_name.clone().unwrap_or_else(|| "their CTO".into());
            let co = d.company_name.clone().unwrap_or_default();
            let (kind, bot, summary, approval) = match i % 8 {
                0 => (ActivityKind::Research, Some(self.crew.lead), format!("Found {co}: Series B in March, hiring 4 SREs"), None),
                1 => (ActivityKind::EmailSent, Some(self.crew.drafter), format!("First email to {contact}: incident reviews at {co}"), Some(cid(9, 100 + i))),
                2 => (ActivityKind::EmailReceived, Some(self.crew.tracker), format!("{contact} replied: “Interested, can we talk next week?”"), None),
                3 => (ActivityKind::StageChange, Some(self.crew.tracker), format!("Moved {} to Replied", d.title), None),
                4 => (ActivityKind::DmSent, Some(self.crew.drafter), format!("LinkedIn note to {contact}"), Some(cid(9, 100 + i))),
                5 => (ActivityKind::Note, None, format!("{contact} prefers async; loop in their platform lead"), None),
                6 => (ActivityKind::Meeting, None, format!("Demo with {contact} and the platform team"), None),
                _ => (ActivityKind::Research, Some(self.crew.lead), format!("{co} posted a postmortem about a 3-hour outage"), None),
            };
            self.activities.push(CrmActivity {
                id: cid(4, i),
                company_id: Some(d.company_id),
                contact_id: d.contact_id,
                deal_id: Some(d.id),
                kind,
                summary,
                body: (i % 8 == 2).then(|| "Thanks for the note. We're rethinking on-call this quarter, so the timing is good. Could you send something short I can share with the team first?".into()),
                url: (i % 8 == 0 || i % 8 == 7).then(|| format!("https://news.example.com/{}", co.to_lowercase().replace(' ', "-"))),
                approval_id: approval,
                bot_id: bot,
                bot_name: None,
                actor_kind: if bot.is_some() { "bot".into() } else { "user".into() },
                occurred_at: now - Duration::minutes(3 + i as i64 * 29),
                created_at: now - Duration::minutes(3 + i as i64 * 29),
            });
        }
        // The newest entry: a draft still waiting in Needs you.
        let d = &self.deals[0];
        self.activities.insert(
            0,
            CrmActivity {
                id: cid(4, 9999),
                company_id: Some(d.company_id),
                contact_id: d.contact_id,
                deal_id: Some(d.id),
                kind: ActivityKind::Note,
                summary: format!("Drafted a first email to {} for your OK", d.contact_name.clone().unwrap_or_default()),
                approval_id: Some(self.waiting_draft),
                bot_id: Some(self.crew.drafter),
                actor_kind: "bot".into(),
                occurred_at: now - Duration::minutes(1),
                created_at: now - Duration::minutes(1),
                ..Default::default()
            },
        );
        let delivery = |n: usize, hook: Uuid, event: &str, status: &str, attempts: i32, err: Option<&str>, m: i64| CrmWebhookDelivery {
            id: cid(6, n),
            webhook_id: Some(hook),
            event: event.into(),
            payload: None,
            status: status.into(),
            attempts,
            next_attempt_at: None,
            last_error: err.map(Into::into),
            created_at: Some(now - Duration::minutes(m)),
            delivered_at: (status == "delivered").then(|| now - Duration::minutes(m)),
        };
        let (zap, attio) = (cid(5, 1), cid(5, 2));
        self.deliveries = vec![
            delivery(1, zap, "deal.stage_changed", "delivered", 1, None, 3),
            delivery(2, zap, "company.created", "delivered", 1, None, 9),
            delivery(3, zap, "contact.created", "delivered", 2, None, 14),
            delivery(4, zap, "activity.created", "delivered", 1, None, 21),
            delivery(5, attio, "contact.do_not_contact", "failed", 6, Some("502 Bad Gateway"), 40),
            delivery(6, attio, "deal.updated", "pending", 3, Some("connection timed out after 10 s"), 12),
        ];
        let all: Vec<String> = crate::crm_model::WEBHOOK_EVENTS.iter().map(|(e, _)| (*e).to_owned()).collect();
        self.webhooks = vec![
            CrmWebhook {
                id: zap,
                url: "https://hooks.zapier.com/hooks/catch/1234567/b8kq2x/".into(),
                events: all,
                enabled: true,
                created_at: Some(now - Duration::days(3)),
                secret: None,
                last_delivery: Some(self.deliveries[0].clone()),
            },
            CrmWebhook {
                id: attio,
                url: "https://crm-sync.example.com/familiar".into(),
                events: vec!["contact.created".into(), "contact.updated".into(), "contact.do_not_contact".into()],
                enabled: true,
                created_at: Some(now - Duration::days(2)),
                secret: None,
                last_delivery: Some(self.deliveries[4].clone()),
            },
        ];
    }

    fn pipeline(&self) -> Vec<PipelineStage> {
        STAGES
            .iter()
            .map(|s| {
                let mut of: Vec<&CrmDeal> = self.deals.iter().filter(|d| d.stage == *s).collect();
                of.sort_by_key(|d| std::cmp::Reverse(d.updated_at));
                PipelineStage {
                    stage: *s,
                    count: of.len() as i64,
                    value_cents: of.iter().filter_map(|d| d.value_cents).sum(),
                    deals: of
                        .iter()
                        .take(100)
                        .map(|d| PipelineDeal {
                            id: d.id,
                            stage: d.stage,
                            title: d.title.clone(),
                            company_id: d.company_id,
                            company_name: d.company_name.clone(),
                            contact_id: d.contact_id,
                            contact_name: d.contact_name.clone(),
                            contact_do_not_contact: d.contact_do_not_contact,
                            stage_changed_at: Some(d.stage_changed_at),
                            value_cents: d.value_cents,
                            currency: d.currency.clone(),
                            next_step: d.next_step.clone(),
                            next_step_at: d.next_step_at,
                        })
                        .collect(),
                }
            })
            .collect()
    }

    /// A record's change log: a teammate's latest edit on top of how it was added (do-not-contact for those).
    fn changes(&self, entity: &str, id: Uuid) -> Vec<CrmChange> {
        let ch = |n: u128, op: &str, before: Value, after: Value, bot: Option<Uuid>, m: i64| CrmChange {
            id: Uuid::from_u128(id.as_u128() ^ (n << 100)),
            entity: entity.into(),
            entity_id: id,
            op: op.into(),
            before: Some(before),
            after: Some(after),
            actor_kind: if bot.is_some() { "bot".into() } else { "user".into() },
            bot_id: bot,
            at: self.now - Duration::minutes(m),
            ..Default::default()
        };
        match entity {
            "company" => vec![
                ch(1, "update", json!({"fit_score": 64, "tags": ["saas"]}), json!({"fit_score": 82, "tags": ["saas", "hiring"]}), Some(self.crew.lead), 42),
                ch(2, "update", json!({"location": null}), json!({"location": "Berlin"}), None, 60 * 26),
                ch(3, "create", Value::Null, json!({}), Some(self.crew.lead), 60 * 24 * 6),
            ],
            "contact" if self.contacts.iter().any(|c| c.id == id && c.do_not_contact) => vec![
                ch(1, "update", json!({"do_not_contact": false}), json!({"do_not_contact": true, "dnc_reason": "x"}), Some(self.crew.tracker), 60 * 30),
                ch(2, "create", Value::Null, json!({}), Some(self.crew.lead), 60 * 24 * 7),
            ],
            "deal" => vec![
                ch(1, "update", json!({"stage": "contacted", "next_step": null}), json!({"stage": "replied", "next_step": "x"}), Some(self.crew.tracker), 95),
                ch(2, "update", json!({"stage": "new"}), json!({"stage": "contacted"}), Some(self.crew.drafter), 60 * 20),
                ch(3, "create", Value::Null, json!({}), Some(self.crew.lead), 60 * 24 * 5),
            ],
            _ => vec![
                ch(1, "update", json!({"title": "Engineer"}), json!({"title": "Staff SRE"}), Some(self.crew.lead), 300),
                ch(2, "create", Value::Null, json!({}), Some(self.crew.lead), 60 * 24 * 7),
            ],
        }
    }

    /// The CRM's reads (`segs` is the path after `/api`).
    pub fn get(&self, segs: &[&str], q: &HashMap<String, String>) -> Option<Value> {
        let uuid = |s: &str| s.parse::<Uuid>().ok();
        let qv = |k: &str| q.get(k).map(String::as_str).filter(|v| !v.is_empty());
        let page = |n: usize| {
            let limit = qv("limit").and_then(|l| l.parse().ok()).unwrap_or(50usize).min(200);
            let offset = qv("offset").and_then(|o| o.parse().ok()).unwrap_or(0usize);
            (offset.min(n), (offset + limit).min(n))
        };
        let text = |hay: &[Option<&str>]| match qv("q") {
            Some(q) => crate::crm_model::matches(q, hay),
            None => true,
        };
        let tag = |tags: &[String]| qv("tag").is_none_or(|t| tags.iter().any(|x| x.eq_ignore_ascii_case(t)));
        Some(match segs {
            ["crm", "companies"] => {
                let mut v: Vec<&CrmCompany> = self
                    .companies
                    .iter()
                    .filter(|c| text(&[Some(&c.name), c.domain.as_deref(), c.industry.as_deref(), c.location.as_deref()]) && tag(&c.tags))
                    .collect();
                match qv("sort") {
                    Some("name") => v.sort_by_key(|c| c.name.to_lowercase()),
                    Some("fit") => v.sort_by_key(|c| std::cmp::Reverse(c.fit_score.unwrap_or(-1))),
                    _ => v.sort_by_key(|x| std::cmp::Reverse(x.updated_at)),
                }
                let (a, b) = page(v.len());
                json!(v[a..b])
            }
            ["crm", "companies", id] => json!(self.companies.iter().find(|c| Some(c.id) == uuid(id))?),
            ["crm", "contacts"] => {
                let co = qv("company_id").and_then(uuid);
                let dnc = qv("dnc").map(|d| d == "true");
                let mut v: Vec<&CrmContact> = self
                    .contacts
                    .iter()
                    .filter(|c| {
                        text(&[Some(&c.name), c.email.as_deref(), c.title.as_deref(), c.company_name.as_deref()])
                            && tag(&c.tags)
                            && co.is_none_or(|co| c.company_id == Some(co))
                            && dnc.is_none_or(|d| c.do_not_contact == d)
                    })
                    .collect();
                match qv("sort") {
                    Some("name") => v.sort_by_key(|c| c.name.to_lowercase()),
                    _ => v.sort_by_key(|x| std::cmp::Reverse(x.updated_at)),
                }
                let (a, b) = page(v.len());
                json!(v[a..b])
            }
            ["crm", "contacts", id] => json!(self.contacts.iter().find(|c| Some(c.id) == uuid(id))?),
            ["crm", "deals"] => {
                let co = qv("company_id").and_then(uuid);
                let stage = qv("stage");
                let mut v: Vec<&CrmDeal> = self
                    .deals
                    .iter()
                    .filter(|d| {
                        text(&[Some(&d.title), d.next_step.as_deref(), d.company_name.as_deref()])
                            && co.is_none_or(|co| d.company_id == co)
                            && stage.is_none_or(|s| d.stage.as_str() == s)
                    })
                    .collect();
                match qv("sort") {
                    Some("name") => v.sort_by_key(|d| d.title.to_lowercase()),
                    _ => v.sort_by_key(|x| std::cmp::Reverse(x.updated_at)),
                }
                let (a, b) = page(v.len());
                json!(v[a..b])
            }
            ["crm", "deals", id] => json!(self.deals.iter().find(|d| Some(d.id) == uuid(id))?),
            ["crm", "activities"] => {
                let (co, ct, dl) = (qv("company_id").and_then(uuid), qv("contact_id").and_then(uuid), qv("deal_id").and_then(uuid));
                let v: Vec<&CrmActivity> = self
                    .activities
                    .iter()
                    .filter(|a| co.is_none_or(|x| a.company_id == Some(x)) && ct.is_none_or(|x| a.contact_id == Some(x)) && dl.is_none_or(|x| a.deal_id == Some(x)))
                    .collect();
                let (a, b) = page(v.len());
                json!(v[a..b])
            }
            ["crm", "pipeline"] => json!(self.pipeline()),
            ["crm", "changes"] => json!(self.changes(qv("entity").unwrap_or("company"), qv("entity_id").and_then(uuid)?)),
            ["crm", "webhooks"] => json!(self.webhooks),
            ["crm", "webhooks", id, "deliveries"] => {
                let id = uuid(id)?;
                json!(self.deliveries.iter().filter(|d| d.webhook_id == Some(id)).collect::<Vec<_>>())
            }
            ["templates"] => json!(self.templates),
            ["templates", "bundles"] => json!([self.bundle]),
            _ => return None,
        })
    }

    /// The writes the screenshots use. `hire`: the fixture's bots and schedules gain the hired crew.
    pub fn write(&mut self, method: &str, segs: &[&str], body: &str) -> Option<(Value, Option<Hire>)> {
        let now = self.now;
        Some(match (method, segs) {
            // A dry run and the real import answer alike (nothing is stored).
            ("POST", ["crm", "import"]) => {
                let rows = crate::crm_model::csv_shape(body).rows as u32;
                let errors = json!([
                    {"row": 14, "message": "domain isn't a host name: “acme dot com”"},
                    {"row": 52, "message": "fit_score must be a whole number"},
                    {"row": 97, "message": "a name is needed"},
                ]);
                (json!({"created": rows.saturating_sub(36), "updated": 31u32.min(rows), "skipped": 2u32.min(rows), "errors": errors}), None)
            }
            ("POST", ["crm", "webhooks"]) => {
                let v: Value = serde_json::from_str(body).ok()?;
                let hook = CrmWebhook {
                    id: cid(5, 10 + self.webhooks.len()),
                    url: v["url"].as_str().unwrap_or_default().into(),
                    events: v["events"].as_array().map(|a| a.iter().filter_map(|e| e.as_str().map(Into::into)).collect()).unwrap_or_default(),
                    enabled: true,
                    created_at: Some(now),
                    secret: None,
                    last_delivery: None,
                };
                self.webhooks.push(hook.clone());
                let shown = CrmWebhook { secret: Some(format!("whsec_{}", "3f9a1c07d2e84b6a9c5e0f17b2d48a6c1e93f0a2b7c64d18e5f2a9b03c7d61e4")), ..hook };
                (json!(shown), None)
            }
            ("POST", ["crm", "webhooks", id, "test"]) => {
                let id = id.parse::<Uuid>().ok()?;
                let d = CrmWebhookDelivery {
                    id: cid(6, 900),
                    webhook_id: Some(id),
                    event: "ping".into(),
                    status: "delivered".into(),
                    attempts: 1,
                    created_at: Some(now),
                    delivered_at: Some(now),
                    ..Default::default()
                };
                (json!(d), None)
            }
            ("PATCH", ["crm", "deals", id]) => {
                let v: Value = serde_json::from_str(body).ok()?;
                let d = self.deals.iter_mut().find(|d| id.parse::<Uuid>().ok() == Some(d.id))?;
                if let Some(s) = v["stage"].as_str().and_then(|s| STAGES.iter().find(|x| x.as_str() == s)) {
                    d.stage = *s;
                    d.stage_changed_at = now;
                    d.updated_at = now;
                }
                (json!(d), None)
            }
            ("POST", ["templates", "bundles", _, "create"]) => {
                let mut bots = Vec::new();
                let mut schedules = Vec::new();
                for (i, t) in self.templates.iter().enumerate() {
                    let id = cid(7, i);
                    let ids: Vec<Uuid> = (0..t.schedules.len()).map(|k| cid(8, i * 10 + k)).collect();
                    for (k, s) in t.schedules.iter().enumerate() {
                        schedules.push(Schedule {
                            id: ids[k],
                            bot_id: id,
                            cron: s.cron.clone(),
                            prompt: s.label.clone(),
                            kind: RunKind::Scheduled,
                            enabled: false,
                            label: Some(s.label.clone()),
                            bot_name: Some(t.name.clone()),
                            ..Default::default()
                        });
                    }
                    bots.push(Bot {
                        id,
                        slug: t.id.clone(),
                        name: t.name.clone(),
                        persona: Some(t.summary.clone()),
                        model: "sonnet".into(),
                        engine: BotEngine::Claude,
                        avatar: t.avatar.clone(),
                        status: Some(BotStatus::Idle),
                        created_at: now,
                        setup: Some(BotSetup {
                            template: Some(t.id.clone()),
                            logins: t.logins.iter().map(|l| SetupLogin { site: l.site.clone(), url: l.url.clone(), done: false }).collect(),
                            schedules: ids,
                            ..Default::default()
                        }),
                        ..Default::default()
                    });
                }
                let hired: Vec<Hired> = bots.iter().map(|b| Hired { bot: b.clone(), first_task: None }).collect();
                (json!({"hired": hired}), Some((bots, schedules)))
            }
            _ => return None,
        })
    }
}

/// The query of a request target, decoded.
pub fn query(target: &str) -> HashMap<String, String> {
    reqwest::Url::parse(&format!("http://bench{target}"))
        .map(|u| u.query_pairs().map(|(k, v)| (k.into_owned(), v.into_owned())).collect())
        .unwrap_or_default()
}

/// A sample file for the import preview: 152 companies.
pub fn sample_csv() -> String {
    let mut s = String::from("name,domain,industry,fit_score,tags,notes\n");
    for i in 0..152 {
        let _ = std::fmt::Write::write_fmt(&mut s, format_args!("Sample {i} Labs,sample{i}.com,B2B SaaS,{},\"saas, eu\",met at a conference\n", 40 + i % 60));
    }
    s
}
