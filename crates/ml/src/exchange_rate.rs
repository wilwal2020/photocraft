//! The US dollar's rate in Norwegian kroner, from Norges Bank's free exchange-rate API (no key):
//! Google bills Nano Banana in USD and the shell shows what it cost in kr. Norges Bank publishes
//! one rate per business day (ECB concertation, about 14:15 CET); [`usd_nok`] asks for the latest
//! one. Endpoint and format from data.norges-bank.no (EXR dataflow, CSV), read 2026-11-07.

use crate::MlError;

/// One day's rate.
#[derive(Clone, Debug, PartialEq)]
pub struct Rate {
    /// Kroner per US dollar.
    pub nok_per_usd: f64,
    /// The day it is for (`YYYY-MM-DD`, Norges Bank's `TIME_PERIOD`).
    pub date: String,
}

#[cfg(not(target_arch = "wasm32"))]
const ENDPOINT: &str = "https://data.norges-bank.no/api/data/EXR/B.USD.NOK.SP?format=csv&lastNObservations=1&locale=en";

/// The latest USD → NOK rate (one HTTPS request, at most 20 s).
#[cfg(not(target_arch = "wasm32"))]
pub fn usd_nok() -> Result<Rate, MlError> {
    let agent: ureq::Agent = ureq::Agent::config_builder().timeout_global(Some(std::time::Duration::from_secs(20))).http_status_as_error(false).build().into();
    let mut answer = agent.get(ENDPOINT).call().map_err(|e| MlError::Inference(format!("couldn't reach Norges Bank ({e})")))?;
    let status = answer.status().as_u16();
    let text = answer
        .body_mut()
        .with_config()
        .limit(1 << 16)
        .read_to_string()
        .map_err(|e| MlError::Inference(format!("Norges Bank's answer didn't arrive whole ({e})")))?;
    if !(200..300).contains(&status) {
        return Err(MlError::Inference(format!("Norges Bank answered {status}")));
    }
    parse(&text).ok_or_else(|| MlError::Inference("Norges Bank's answer has no USD rate".into()))
}

/// The web build has no network access here.
#[cfg(target_arch = "wasm32")]
pub fn usd_nok() -> Result<Rate, MlError> {
    Err(MlError::Unsupported)
}

/// The last rate in Norges Bank's CSV answer (`;`-separated, a header row naming `TIME_PERIOD`,
/// `OBS_VALUE` and `UNIT_MULT`), or `None` when it has none that makes sense.
pub fn parse(csv: &str) -> Option<Rate> {
    let mut lines = csv.lines().map(|l| l.trim_start_matches('\u{feff}').trim()).filter(|l| !l.is_empty());
    let header: Vec<&str> = lines.next()?.split(';').map(|c| c.trim_matches('"')).collect();
    let col = |name: &str| header.iter().position(|h| *h == name);
    let (date, value, mult) = (col("TIME_PERIOD")?, col("OBS_VALUE")?, col("UNIT_MULT"));
    let row: Vec<&str> = lines.next_back()?.split(';').map(|c| c.trim_matches('"')).collect();
    let mut rate: f64 = row.get(value)?.replace(',', ".").parse().ok()?;
    // UNIT_MULT is the power of ten the value is quoted per (0 for USD: kroner per dollar).
    if let Some(m) = mult.and_then(|i| row.get(i)).and_then(|m| m.parse::<i32>().ok()) {
        rate /= 10f64.powi(m);
    }
    let date = row.get(date)?.to_string();
    (rate.is_finite() && rate > 0.0 && !date.is_empty()).then_some(Rate { nok_per_usd: rate, date })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Norges Bank's answer of 2026-11-07, as it came.
    const ANSWER: &str = "FREQ;Frequency;BASE_CUR;Base Currency;QUOTE_CUR;Quote Currency;TENOR;Tenor;DECIMALS;CALCULATED;UNIT_MULT;Unit Multiplier;COLLECTION;Collection Indicator;TIME_PERIOD;OBS_VALUE\n\
B;Business;USD;US dollar;NOK;Norwegian krone;SP;Spot;4;false;0;Units;C;ECB concertation time 14:15 CET;2026-11-06;9.4277\n";

    #[test]
    fn reads_the_rate_and_its_day() {
        assert_eq!(parse(ANSWER), Some(Rate { nok_per_usd: 9.4277, date: "2026-11-06".into() }));
        // Quoted per 100 units (as Norges Bank quotes some currencies).
        let per_100 = ANSWER.replace(";0;Units;", ";2;Hundreds;").replace("9.4277", "942.77");
        assert!(parse(&per_100).is_some_and(|r| (r.nok_per_usd - 9.4277).abs() < 1e-9));
    }

    #[test]
    fn refuses_answers_without_a_usable_rate() {
        assert_eq!(parse(""), None);
        assert_eq!(parse("FREQ;TIME_PERIOD\nB;2026-11-06"), None);
        assert_eq!(parse(&ANSWER.replace("9.4277", "NaN")), None);
        assert_eq!(parse(&ANSWER.replace("9.4277", "-1")), None);
        assert_eq!(parse("<html>maintenance</html>"), None);
    }
}
