//! HVAC loop topology (plant, condenser, and air loops) extracted from the IDF.
//!
//! Everything needed for a loop schematic is explicit in the IDF: each loop
//! object names its supply/demand BranchList, each Branch lists components in
//! flow order with inlet/outlet nodes, and Connector:Splitter/Mixer describe
//! the parallel section. Air loop demand sides come from the SupplyPath /
//! ReturnPath objects plus node-name matching against zone equipment. No .bnd
//! file is required.

use crate::idf::IdfObject;
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoopKind {
    Plant,
    Condenser,
    Air,
}

impl LoopKind {
    pub fn label(self) -> &'static str {
        match self {
            LoopKind::Plant => "Plant",
            LoopKind::Condenser => "Condenser",
            LoopKind::Air => "Air",
        }
    }
}

/// A sub-object referenced by a component (fan and coils of a unitary system,
/// controllers of an OA system, equipment of a zone, ...).
#[derive(Debug, Clone)]
pub struct ChildRef {
    pub class: String,
    pub name: String,
    pub raw: String,
    pub line: usize,
}

/// One box in the schematic: a branch component, a zone, or a terminal unit.
#[derive(Debug, Clone, Default)]
pub struct Component {
    pub class: String,
    pub name: String,
    pub inlet: String,
    pub outlet: String,
    /// Raw IDF text of the referenced object; empty if not found in the file.
    pub raw: String,
    pub line: usize,
    pub found: bool,
    pub children: Vec<ChildRef>,
    /// Key sizing values in IP units, e.g. ("Capacity", "500 tons").
    pub specs: Vec<(&'static str, String)>,
    /// Name of the compound parent this box was expanded out of
    /// (e.g. an AirLoopHVAC:UnitarySystem); drawn as a bracket around the run.
    pub group: Option<String>,
    /// Parallel tap within its group (a WaterUse:Connections fixture) rather
    /// than a series stage; consecutive stacked boxes of one group are drawn
    /// as a vertical stack between fan-out/fan-in tees.
    pub stacked: bool,
}

#[derive(Debug, Clone)]
pub struct BranchView {
    pub name: String,
    pub components: Vec<Component>,
}

/// One half-loop. Flow order: inlet node → series_in branches → splitter →
/// parallel branches → mixer → series_out branches → outlet node. When there
/// is no splitter every branch is in `series_in`.
#[derive(Debug, Clone, Default)]
pub struct Side {
    pub label: String,
    pub inlet_node: String,
    pub outlet_node: String,
    pub series_in: Vec<BranchView>,
    pub parallel: Vec<BranchView>,
    pub series_out: Vec<BranchView>,
    pub splitter: Option<Component>,
    pub mixer: Option<Component>,
    /// Air streams that leave the main flow path (an OA system's relief /
    /// exhaust stream), drawn as their own runs below the main line. The
    /// branch name labels the run.
    pub aux: Vec<BranchView>,
}

impl Side {
    pub fn branch_count(&self) -> usize {
        self.series_in.len() + self.parallel.len() + self.series_out.len()
    }
}

#[derive(Debug, Clone)]
pub struct HvacLoop {
    pub kind: LoopKind,
    pub name: String,
    /// Supply side first, then demand side.
    pub sides: Vec<Side>,
    pub raw: String,
    pub line: usize,
    pub warnings: Vec<String>,
}

// --- object index -----------------------------------------------------------

struct Index<'a> {
    objects: &'a [IdfObject],
    /// lowercase class -> object indices
    by_class: HashMap<String, Vec<usize>>,
    /// (lowercase class, lowercase name) -> object index
    by_name: HashMap<(String, String), usize>,
}

fn eq(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

impl<'a> Index<'a> {
    fn new(objects: &'a [IdfObject]) -> Self {
        let mut by_class: HashMap<String, Vec<usize>> = HashMap::new();
        let mut by_name = HashMap::new();
        for (i, o) in objects.iter().enumerate() {
            let c = o.class.to_ascii_lowercase();
            by_class.entry(c.clone()).or_default().push(i);
            by_name.insert((c, o.field(0).to_ascii_lowercase()), i);
        }
        Self {
            objects,
            by_class,
            by_name,
        }
    }

    fn find(&self, class: &str, name: &str) -> Option<&'a IdfObject> {
        self.by_name
            .get(&(class.to_ascii_lowercase(), name.to_ascii_lowercase()))
            .map(|&i| &self.objects[i])
    }

    fn all(&self, class: &str) -> impl Iterator<Item = &'a IdfObject> + '_ {
        self.by_class
            .get(&class.to_ascii_lowercase())
            .into_iter()
            .flatten()
            .map(|&i| &self.objects[i])
    }

    fn has_class(&self, class: &str) -> bool {
        self.by_class.contains_key(&class.to_ascii_lowercase())
    }

    /// A node field may hold either a node name or a NodeList name.
    fn resolve_nodes(&self, name: &str) -> Vec<String> {
        if name.is_empty() {
            return Vec::new();
        }
        match self.find("NodeList", name) {
            Some(nl) => nl.fields[1..]
                .iter()
                .filter(|f| !f.is_empty())
                .cloned()
                .collect(),
            None => vec![name.to_string()],
        }
    }
}

// --- component construction -------------------------------------------------

/// Collect (object type, object name) sub-references from `obj`. IDF encodes
/// these as a field holding a class name (always containing ':') followed by
/// the object's name.
fn collect_children(idx: &Index, obj: &IdfObject, out: &mut Vec<ChildRef>) {
    // OA systems reference their controllers and equipment through name-only
    // list objects, so the type/name pair scan below can't see them; expand
    // the lists instead.
    if eq(&obj.class, "AirLoopHVAC:OutdoorAirSystem") {
        for (list_class, field) in [
            ("AirLoopHVAC:ControllerList", 1),
            ("AirLoopHVAC:OutdoorAirSystem:EquipmentList", 2),
        ] {
            if let Some(list) = idx.find(list_class, obj.field(field)) {
                collect_children(idx, list, out);
            }
        }
    }
    for i in 0..obj.fields.len().saturating_sub(1) {
        let class = obj.field(i);
        if class.contains(':') && idx.has_class(class) {
            if let Some(child) = idx.find(class, obj.field(i + 1)) {
                if out
                    .iter()
                    .any(|c| eq(&c.class, &child.class) && eq(&c.name, child.field(0)))
                {
                    continue;
                }
                out.push(ChildRef {
                    class: child.class.clone(),
                    name: child.field(0).to_string(),
                    raw: child.raw.clone(),
                    line: child.line,
                });
            }
        }
    }
}

// --- equipment sizing specs -------------------------------------------------

#[derive(Clone, Copy)]
enum Unit {
    /// W → tons of refrigeration
    Tons,
    /// W → kBtu/h
    KBtuH,
    /// m3/s water → GPM
    Gpm,
    /// Pa pump head → ft of water
    FtHead,
    /// m3/s air → CFM
    Cfm,
    /// Pa fan pressure → in. w.c.
    InH2O,
    /// m3 → gal
    Gal,
    /// fraction → %
    Pct,
    /// °C → °F
    DegF,
    /// W → kW
    Kw,
    /// W → tons and kBtu/h together
    TonsBtu,
    /// dimensionless (COP, ...)
    Plain,
}

/// Sizing fields worth surfacing, per class: (field index, label, unit).
/// Indices are 0-based with the object name at index 0, verified against the
/// 24.2 IDD field order.
fn spec_defs(class: &str) -> &'static [(usize, &'static str, Unit)] {
    use Unit::*;
    match class.to_ascii_lowercase().as_str() {
        "chiller:electric:eir" | "chiller:electric:reformulatedeir" => {
            &[(1, "Capacity", Tons), (5, "CHW flow", Gpm)]
        }
        "chiller:constantcop" => &[(1, "Capacity", Tons), (3, "CHW flow", Gpm)],
        "chiller:electric" => &[(2, "Capacity", Tons), (14, "CHW flow", Gpm)],
        "chiller:absorption" => &[(1, "Capacity", Tons), (11, "CHW flow", Gpm)],
        "chiller:absorption:indirect" => &[(1, "Capacity", Tons), (13, "CHW flow", Gpm)],
        "boiler:hotwater" => &[(2, "Capacity", KBtuH), (6, "Flow", Gpm)],
        "boiler:steam" => &[(5, "Capacity", KBtuH)],
        "pump:variablespeed" | "pump:constantspeed" => &[(3, "Flow", Gpm), (4, "Head", FtHead)],
        "headeredpumps:variablespeed" | "headeredpumps:constantspeed" => {
            &[(3, "Total flow", Gpm), (6, "Head", FtHead)]
        }
        "coolingtower:singlespeed" | "coolingtower:twospeed" => &[(3, "Water flow", Gpm)],
        "coolingtower:variablespeed" => &[(8, "Water flow", Gpm)],
        "coil:cooling:water" => &[(2, "Water flow", Gpm)],
        "coil:cooling:dx:singlespeed" | "coil:cooling:dx:twospeed" => {
            &[(2, "Capacity", TonsBtu), (4, "COP", Plain)]
        }
        "coil:cooling:dx:variablespeed" => &[(5, "Capacity", TonsBtu)],
        "coil:heating:water" => &[(3, "Water flow", Gpm), (9, "Capacity", KBtuH)],
        "districtcooling" => &[(3, "Capacity", Tons)],
        "districtheating" | "districtheating:water" | "districtheating:steam" => {
            &[(3, "Capacity", KBtuH)]
        }
        "coil:heating:electric" => &[(3, "Capacity", KBtuH)],
        "coil:heating:fuel" => &[(4, "Capacity", KBtuH)],
        "heatexchanger:airtoair:sensibleandlatent" => &[
            (2, "Flow", Cfm),
            (3, "Sens eff", Pct),
            (4, "Lat eff", Pct),
        ],
        "waterheater:mixed" => &[(1, "Tank", Gal), (6, "Heater", KBtuH)],
        "wateruse:equipment" => &[(2, "Peak flow", Gpm)],
        "fan:constantvolume" | "fan:onoff" => &[
            (4, "Flow", Cfm),
            (3, "ΔP", InH2O),
            (5, "Motor eff", Pct),
            (2, "Fan eff", Pct),
        ],
        "fan:variablevolume" => &[
            (4, "Flow", Cfm),
            (3, "ΔP", InH2O),
            (8, "Motor eff", Pct),
            (2, "Fan eff", Pct),
        ],
        "fan:systemmodel" => &[
            (4, "Flow", Cfm),
            (7, "ΔP", InH2O),
            (10, "Power", Kw),
            (8, "Motor eff", Pct),
            (14, "Fan eff", Pct),
        ],
        _ => &[],
    }
}

/// Format with thousands separators and just enough precision.
fn fmt_num(v: f64) -> String {
    let s = if v >= 100.0 {
        format!("{:.0}", v)
    } else if v >= 10.0 {
        format!("{:.1}", v)
    } else {
        format!("{:.2}", v)
    };
    let s = if s.contains('.') {
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    } else {
        s
    };
    let (int, frac) = s.split_once('.').map_or((s.as_str(), None), |(i, f)| (i, Some(f)));
    let mut out = String::new();
    for (n, ch) in int.chars().enumerate() {
        if n > 0 && (int.len() - n) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    if let Some(f) = frac {
        out.push('.');
        out.push_str(f);
    }
    out
}

fn fmt_spec(raw: &str, unit: Unit) -> Option<String> {
    let t = raw.trim();
    if t.is_empty() {
        return None;
    }
    if t.eq_ignore_ascii_case("autosize") || t.eq_ignore_ascii_case("autocalculate") {
        return Some("autosized".to_string());
    }
    let v: f64 = t.parse().ok()?;
    let (v, suffix) = match unit {
        Unit::Tons => (v / 3516.85, "tons"),
        Unit::KBtuH => (v / 293.071, "kBtu/h"),
        Unit::Gpm => (v * 15850.32, "GPM"),
        Unit::FtHead => (v / 2989.07, "ft"),
        Unit::Cfm => (v * 2118.88, "CFM"),
        Unit::InH2O => (v / 249.089, "in. w.c."),
        Unit::Gal => (v * 264.172, "gal"),
        Unit::Pct => (v * 100.0, "%"),
        Unit::DegF => (v * 9.0 / 5.0 + 32.0, "°F"),
        Unit::Kw => return Some(fmt_power(v)),
        Unit::TonsBtu => {
            return Some(format!(
                "{} tons ({} kBtu/h)",
                fmt_num(v / 3516.85),
                fmt_num(v / 293.071)
            ));
        }
        Unit::Plain => return Some(fmt_num(v)),
    };
    if matches!(unit, Unit::Pct | Unit::DegF) {
        return Some(format!("{}{}", fmt_num(v), suffix));
    }
    Some(format!("{} {}", fmt_num(v), suffix))
}

/// Power in both kW and hp, from watts.
fn fmt_power(w: f64) -> String {
    format!("{} kW ({} hp)", fmt_num(w / 1000.0), fmt_num(w / 745.7))
}

/// Simple fans (OnOff/ConstantVolume/VariableVolume) have no power field;
/// EnergyPlus computes design power as flow · ΔP / total efficiency.
fn fan_power_spec(obj: &IdfObject) -> Option<String> {
    let flow = obj.field_f64(4)?;
    let dp = obj.field_f64(3)?;
    let eff = obj.field_f64(2)?;
    if eff <= 0.0 {
        return None;
    }
    Some(fmt_power(flow * dp / eff))
}

fn make_component(idx: &Index, class: &str, name: &str, inlet: &str, outlet: &str) -> Component {
    let mut c = Component {
        class: class.to_string(),
        name: name.to_string(),
        inlet: inlet.to_string(),
        outlet: outlet.to_string(),
        ..Default::default()
    };
    if let Some(obj) = idx.find(class, name) {
        c.raw = obj.raw.clone();
        c.line = obj.line;
        c.found = true;
        collect_children(idx, obj, &mut c.children);
        for &(i, label, unit) in spec_defs(&obj.class) {
            if let Some(v) = fmt_spec(obj.field(i), unit) {
                c.specs.push((label, v));
            }
        }
        let lc = obj.class.to_ascii_lowercase();
        if matches!(
            lc.as_str(),
            "fan:onoff" | "fan:constantvolume" | "fan:variablevolume"
        ) {
            if let Some(p) = fan_power_spec(obj) {
                c.specs.insert(2.min(c.specs.len()), ("Power", p));
            }
        }
        if lc == "coil:cooling:dx" {
            dx_coil_specs(idx, obj, &mut c.specs);
        }
        if lc == "waterheater:mixed" {
            push_schedule_temp(idx, obj.field(2), "Setpoint", &mut c.specs);
        }
        if lc == "wateruse:equipment" {
            push_schedule_temp(idx, obj.field(4), "Target temp", &mut c.specs);
        }
    }
    c
}

/// Temperature setpoints live behind a schedule name; a Schedule:Constant
/// resolves to a single value worth showing on the box.
fn push_schedule_temp(
    idx: &Index,
    sched: &str,
    label: &'static str,
    specs: &mut Vec<(&'static str, String)>,
) {
    if let Some(sc) = idx.find("Schedule:Constant", sched) {
        if let Some(v) = fmt_spec(sc.field(2), Unit::DegF) {
            specs.push((label, v));
        }
    }
}

/// New-style DX coil: capacity and COP live two references deep, on the
/// CurveFit performance's base operating mode and its nominal speed.
fn dx_coil_specs(idx: &Index, coil: &IdfObject, specs: &mut Vec<(&'static str, String)>) {
    let Some(perf) = idx.find("Coil:Cooling:DX:CurveFit:Performance", coil.field(7)) else {
        return;
    };
    let Some(mode) = idx.find("Coil:Cooling:DX:CurveFit:OperatingMode", perf.field(11)) else {
        return;
    };
    if let Some(v) = fmt_spec(mode.field(1), Unit::TonsBtu) {
        specs.push(("Capacity", v));
    }
    // Speed names start at field 12; blank nominal speed number means the
    // highest speed.
    let speeds: Vec<&str> = mode.fields[12.min(mode.fields.len())..]
        .iter()
        .map(String::as_str)
        .filter(|s| !s.is_empty())
        .collect();
    let nominal = mode
        .field_f64(11)
        .map(|n| (n as usize).clamp(1, speeds.len().max(1)) - 1)
        .unwrap_or(speeds.len().saturating_sub(1));
    if let Some(name) = speeds.get(nominal) {
        if let Some(speed) = idx.find("Coil:Cooling:DX:CurveFit:Speed", name) {
            if let Some(v) = fmt_spec(speed.field(5), Unit::Plain) {
                specs.push(("COP", v));
            }
        }
    }
}

/// Expand an AirLoopHVAC:UnitarySystem into its fan/coil chain, in flow order
/// (fan placement decides where the fan sits; supplemental coil is last). The
/// unitary's branch inlet/outlet nodes go to the first/last child; interior
/// connections use auto-generated nodes the IDF doesn't name, so those stay
/// blank. Each child carries `group` (and a back-reference) to the unitary.
fn expand_unitary(idx: &Index, unitary: &Component, obj: &IdfObject) -> Vec<Component> {
    let fan = (obj.field(7), obj.field(8));
    let blow_through = !obj.field(9).eq_ignore_ascii_case("DrawThrough");
    let heat = (obj.field(11), obj.field(12));
    let cool = (obj.field(14), obj.field(15));
    let supp = (obj.field(19), obj.field(20));

    let mut order: Vec<(&str, &str)> = Vec::new();
    if blow_through && !fan.1.is_empty() {
        order.push(fan);
    }
    for pair in [cool, heat] {
        if !pair.1.is_empty() {
            order.push(pair);
        }
    }
    if !blow_through && !fan.1.is_empty() {
        order.push(fan);
    }
    if !supp.1.is_empty() {
        order.push(supp);
    }
    if order.is_empty() {
        return vec![unitary.clone()];
    }

    let n = order.len();
    order
        .into_iter()
        .enumerate()
        .map(|(i, (class, name))| {
            let inlet = if i == 0 { unitary.inlet.as_str() } else { "" };
            let outlet = if i == n - 1 { unitary.outlet.as_str() } else { "" };
            let mut c = make_component(idx, class, name, inlet, outlet);
            c.group = Some(unitary.name.clone());
            c.children.push(ChildRef {
                class: unitary.class.clone(),
                name: unitary.name.clone(),
                raw: unitary.raw.clone(),
                line: unitary.line,
            });
            c
        })
        .collect()
}

/// Expand a WaterUse:Connections into its WaterUse:Equipment fixtures. The
/// fixtures are parallel draws sharing the connections' plant nodes, so every
/// box carries the branch inlet/outlet and is marked `stacked` for parallel
/// rendering; each shows its peak flow and target temperature and carries
/// `group` (and a back-reference) to the connections object.
fn expand_water_use(idx: &Index, conn: &Component, obj: &IdfObject) -> Vec<Component> {
    // Equipment names start at field 10 and are name-only references.
    let names: Vec<&str> = obj.fields[10.min(obj.fields.len())..]
        .iter()
        .map(String::as_str)
        .filter(|s| !s.is_empty())
        .collect();
    if names.is_empty() {
        return vec![conn.clone()];
    }
    names
        .into_iter()
        .map(|name| {
            let mut c = make_component(idx, "WaterUse:Equipment", name, &conn.inlet, &conn.outlet);
            c.stacked = true;
            c.group = Some(conn.name.clone());
            c.children.push(ChildRef {
                class: conn.class.clone(),
                name: conn.name.clone(),
                raw: conn.raw.clone(),
                line: conn.line,
            });
            c
        })
        .collect()
}

/// Primary-air inlet/outlet node field indices for equipment that can appear
/// in an AirLoopHVAC:OutdoorAirSystem:EquipmentList, per the 24.2 IDD. For the
/// air-to-air heat exchangers these are the supply (outdoor-air) side; the
/// exhaust side of a SensibleAndLatent unit is fields 9/10.
fn air_nodes(class: &str) -> Option<(usize, usize)> {
    match class.to_ascii_lowercase().as_str() {
        "fan:systemmodel" => Some((2, 3)),
        "fan:constantvolume" | "fan:onoff" => Some((7, 8)),
        "fan:variablevolume" => Some((15, 16)),
        "coil:heating:electric" => Some((4, 5)),
        "coil:heating:fuel" => Some((5, 6)),
        "coil:heating:water" => Some((6, 7)),
        "coil:cooling:water" => Some((11, 12)),
        "coilsystem:cooling:dx" => Some((2, 3)),
        "heatexchanger:airtoair:sensibleandlatent" => Some((7, 8)),
        "heatexchanger:airtoair:flatplate" => Some((11, 12)),
        "humidifier:steam:electric" => Some((6, 7)),
        "airloophvac:unitarysystem" => Some((5, 6)),
        _ => None,
    }
}

/// A node-field lookup (`air_nodes` or `exhaust_nodes`).
type NodeFields = fn(&str) -> Option<(usize, usize)>;

/// Exhaust/secondary-side inlet/outlet node field indices for heat exchangers
/// that sit across both streams of an OA system.
fn exhaust_nodes(class: &str) -> Option<(usize, usize)> {
    match class.to_ascii_lowercase().as_str() {
        "heatexchanger:airtoair:sensibleandlatent" => Some((9, 10)),
        "heatexchanger:airtoair:flatplate" => Some((13, 14)),
        _ => None,
    }
}

/// Expand an AirLoopHVAC:OutdoorAirSystem into its equipment. Unlike the
/// unitary system the interior nodes are all named in the IDF, so each box
/// carries its real inlet/outlet.
///
/// The first returned list is the outdoor-air intake path in flow order:
/// the intake chain (traced upstream from the mixer's OA inlet node), then
/// unplaceable equipment in list order, then the OutdoorAir:Mixer, whose
/// return-air inlet and mixed-air outlet are the branch connections. The OA
/// controllers are surfaced on the mixer box.
///
/// The second list is the relief/exhaust stream, traced downstream from the
/// mixer's relief node. A heat exchanger sits across both streams, so it
/// appears again here with its exhaust-side nodes.
fn expand_oa_system(
    idx: &Index,
    oa: &Component,
    obj: &IdfObject,
) -> (Vec<Component>, Vec<Component>) {
    let Some(list) = idx.find("AirLoopHVAC:OutdoorAirSystem:EquipmentList", obj.field(2)) else {
        return (vec![oa.clone()], Vec::new());
    };
    let mut items: Vec<(String, String)> = Vec::new();
    let mut i = 1;
    while i + 1 < list.fields.len() {
        let (c, n) = (list.field(i), list.field(i + 1));
        if !c.is_empty() && !n.is_empty() {
            items.push((c.to_string(), n.to_string()));
        }
        i += 2;
    }
    let mixer = items
        .iter()
        .position(|(c, _)| eq(c, "OutdoorAir:Mixer"))
        .map(|p| items.remove(p));
    let mixer_obj = mixer.as_ref().and_then(|(c, n)| idx.find(c, n));

    let nodes = |class: &str, name: &str, which: NodeFields| {
        match (which(class), idx.find(class, name)) {
            (Some((i, o)), Some(eq_obj)) => {
                (eq_obj.field(i).to_string(), eq_obj.field(o).to_string())
            }
            _ => (String::new(), String::new()),
        }
    };
    let mut used = vec![false; items.len()];

    // Intake chain: walk upstream from the mixer's outdoor air inlet.
    let mut intake: Vec<usize> = Vec::new();
    if let Some(mx) = mixer_obj {
        let mut node = mx.field(2).to_string();
        while !node.is_empty() {
            let Some(p) = (0..items.len()).find(|&p| {
                !used[p] && eq(&nodes(&items[p].0, &items[p].1, air_nodes).1, &node)
            }) else {
                break;
            };
            used[p] = true;
            node = nodes(&items[p].0, &items[p].1, air_nodes).0;
            intake.insert(0, p);
        }
    }

    // Relief stream: walk downstream from the mixer's relief node. A heat
    // exchanger already placed on the intake matches through its exhaust-side
    // nodes; anything else (an exhaust fan) through its ordinary air nodes.
    let mut relief: Vec<Component> = Vec::new();
    if let Some(mx) = mixer_obj {
        let mut node = mx.field(3).to_string();
        let mut in_relief = vec![false; items.len()];
        while !node.is_empty() {
            let hx = (0..items.len()).find(|&p| {
                !in_relief[p] && eq(&nodes(&items[p].0, &items[p].1, exhaust_nodes).0, &node)
            });
            let plain = || {
                (0..items.len()).find(|&p| {
                    !in_relief[p]
                        && !used[p]
                        && eq(&nodes(&items[p].0, &items[p].1, air_nodes).0, &node)
                })
            };
            let (p, which): (usize, NodeFields) = match hx {
                Some(p) => (p, exhaust_nodes),
                None => match plain() {
                    Some(p) => {
                        used[p] = true;
                        (p, air_nodes)
                    }
                    None => break,
                },
            };
            in_relief[p] = true;
            let (inlet, outlet) = nodes(&items[p].0, &items[p].1, which);
            node = outlet.clone();
            relief.push(make_component(idx, &items[p].0, &items[p].1, &inlet, &outlet));
        }
    }

    let mut comps: Vec<Component> = intake
        .into_iter()
        .chain((0..items.len()).filter(|&p| !used[p]))
        .map(|p| {
            let (c, n) = &items[p];
            let (inlet, outlet) = nodes(c, n, air_nodes);
            make_component(idx, c, n, &inlet, &outlet)
        })
        .collect();
    if let Some((mc, mn)) = &mixer {
        let (inlet, outlet) = match mixer_obj {
            Some(mx) => (mx.field(4).to_string(), mx.field(1).to_string()),
            None => (String::new(), String::new()),
        };
        let mut c = make_component(idx, mc, mn, &inlet, &outlet);
        if let Some(cl) = idx.find("AirLoopHVAC:ControllerList", obj.field(1)) {
            collect_children(idx, cl, &mut c.children);
        }
        comps.push(c);
    }
    if comps.is_empty() {
        return (vec![oa.clone()], Vec::new());
    }
    for c in comps.iter_mut().chain(&mut relief) {
        c.group = Some(oa.name.clone());
        c.children.push(ChildRef {
            class: oa.class.clone(),
            name: oa.name.clone(),
            raw: oa.raw.clone(),
            line: oa.line,
        });
    }
    (comps, relief)
}

/// Does this branch field start the component quads? Component object types
/// always contain ':' (Pump:VariableSpeed, Coil:Cooling:Water, ...).
fn looks_like_component_type(s: &str) -> bool {
    s.contains(':') || eq(s, "Duct")
}

fn branch_components(idx: &Index, branch: &IdfObject, aux: &mut Vec<BranchView>) -> Vec<Component> {
    let f = &branch.fields;
    // Skip leading non-component fields: pressure drop curve name, and in
    // older IDFs a maximum flow rate.
    let mut i = 1;
    while i < f.len() && i <= 3 && !looks_like_component_type(&f[i]) {
        i += 1;
    }
    let mut comps = Vec::new();
    while i + 1 < f.len() {
        let c = make_component(
            idx,
            &f[i],
            &f[i + 1],
            f.get(i + 2).map(String::as_str).unwrap_or(""),
            f.get(i + 3).map(String::as_str).unwrap_or(""),
        );
        if eq(&c.class, "AirLoopHVAC:UnitarySystem") && c.found {
            let obj = idx.find(&c.class, &c.name).expect("found implies present");
            comps.extend(expand_unitary(idx, &c, obj));
        } else if eq(&c.class, "WaterUse:Connections") && c.found {
            let obj = idx.find(&c.class, &c.name).expect("found implies present");
            comps.extend(expand_water_use(idx, &c, obj));
        } else if eq(&c.class, "AirLoopHVAC:OutdoorAirSystem") && c.found {
            let obj = idx.find(&c.class, &c.name).expect("found implies present");
            let (main, relief) = expand_oa_system(idx, &c, obj);
            if !relief.is_empty() {
                aux.push(BranchView {
                    name: format!("{} relief", c.name),
                    components: relief,
                });
            }
            comps.extend(main);
        } else {
            comps.push(c);
        }
        i += 4;
    }
    comps
}

// --- side assembly ----------------------------------------------------------

/// Node continuity within a series of components: each outlet should feed the
/// next inlet. Mismatches are exactly what the .bnd would flag.
fn check_series(loop_name: &str, branch: &BranchView, warnings: &mut Vec<String>) {
    for w in branch.components.windows(2) {
        let (a, b) = (&w[0], &w[1]);
        // Boxes expanded from the same compound parent are not necessarily one
        // series path (an OA system interleaves its intake and relief streams),
        // so continuity only applies across ordinary connections.
        let same_group = a.group.is_some() && a.group == b.group;
        if !same_group && !a.outlet.is_empty() && !b.inlet.is_empty() && !eq(&a.outlet, &b.inlet) {
            warnings.push(format!(
                "{loop_name}, branch \"{}\": outlet of \"{}\" ({}) does not match inlet of \"{}\" ({})",
                branch.name, a.name, a.outlet, b.name, b.inlet
            ));
        }
        if !a.found {
            warnings.push(format!(
                "{loop_name}, branch \"{}\": component {} \"{}\" not found in file",
                branch.name, a.class, a.name
            ));
        }
    }
    if let Some(last) = branch.components.last() {
        if !last.found {
            warnings.push(format!(
                "{loop_name}, branch \"{}\": component {} \"{}\" not found in file",
                branch.name, last.class, last.name
            ));
        }
    }
}

fn build_side(
    idx: &Index,
    loop_name: &str,
    label: &str,
    inlet: &str,
    outlet: &str,
    branch_list: &str,
    connector_list: &str,
    warnings: &mut Vec<String>,
) -> Side {
    let mut side = Side {
        label: label.to_string(),
        inlet_node: inlet.to_string(),
        outlet_node: outlet.to_string(),
        ..Default::default()
    };

    let mut branches: Vec<BranchView> = Vec::new();
    match idx.find("BranchList", branch_list) {
        Some(bl) => {
            for name in bl.fields[1..].iter().filter(|f| !f.is_empty()) {
                match idx.find("Branch", name) {
                    Some(br) => branches.push(BranchView {
                        name: name.clone(),
                        components: branch_components(idx, br, &mut side.aux),
                    }),
                    None => warnings.push(format!(
                        "{loop_name} ({label}): Branch \"{name}\" not found"
                    )),
                }
            }
        }
        None if !branch_list.is_empty() => warnings.push(format!(
            "{loop_name} ({label}): BranchList \"{branch_list}\" not found"
        )),
        None => {}
    }
    for b in &branches {
        check_series(loop_name, b, warnings);
    }

    // Locate the splitter and mixer via the connector list.
    let mut splitter: Option<&IdfObject> = None;
    let mut mixer: Option<&IdfObject> = None;
    if let Some(cl) = idx.find("ConnectorList", connector_list) {
        let mut i = 1;
        while i + 1 < cl.fields.len() {
            let (ctype, cname) = (cl.field(i), cl.field(i + 1));
            match idx.find(ctype, cname) {
                Some(o) if ctype.to_ascii_lowercase().contains("splitter") => splitter = Some(o),
                Some(o) if ctype.to_ascii_lowercase().contains("mixer") => mixer = Some(o),
                Some(_) => {}
                None if !cname.is_empty() => warnings.push(format!(
                    "{loop_name} ({label}): connector {ctype} \"{cname}\" not found"
                )),
                None => {}
            }
            i += 2;
        }
    }

    let Some(sp) = splitter else {
        side.series_in = branches;
        return side;
    };

    let sp_inlet = sp.field(1);
    let sp_outlets: Vec<&str> = sp.fields[2..].iter().map(String::as_str).collect();
    let mx_outlet = mixer.map(|m| m.field(1)).unwrap_or("");
    let take = |branches: &mut Vec<BranchView>, name: &str| -> Option<BranchView> {
        branches
            .iter()
            .position(|b| eq(&b.name, name))
            .map(|i| branches.remove(i))
    };

    match take(&mut branches, sp_inlet) {
        Some(b) => side.series_in.push(b),
        None => warnings.push(format!(
            "{loop_name} ({label}): splitter inlet branch \"{sp_inlet}\" not in branch list"
        )),
    }
    for name in sp_outlets {
        match take(&mut branches, name) {
            Some(b) => side.parallel.push(b),
            None => warnings.push(format!(
                "{loop_name} ({label}): splitter outlet branch \"{name}\" not in branch list"
            )),
        }
    }
    if !mx_outlet.is_empty() {
        match take(&mut branches, mx_outlet) {
            Some(b) => side.series_out.push(b),
            None => warnings.push(format!(
                "{loop_name} ({label}): mixer outlet branch \"{mx_outlet}\" not in branch list"
            )),
        }
    }
    for b in branches {
        warnings.push(format!(
            "{loop_name} ({label}): branch \"{}\" not referenced by splitter/mixer",
            b.name
        ));
        side.series_out.push(b);
    }

    side.splitter = Some(make_component(idx, &sp.class, sp.field(0), sp_inlet, ""));
    if let Some(m) = mixer {
        side.mixer = Some(make_component(idx, &m.class, m.field(0), "", m.field(1)));
    }
    side
}

// --- air loop demand side ---------------------------------------------------

struct ZoneConn<'a> {
    obj: &'a IdfObject,
    zone: String,
    inlets: Vec<String>,
    returns: Vec<String>,
    equip_list: String,
}

fn zone_component(idx: &Index, zc: &ZoneConn, inlet: &str) -> Component {
    let mut c = Component {
        class: "Zone".to_string(),
        name: zc.zone.clone(),
        inlet: inlet.to_string(),
        outlet: zc.returns.first().cloned().unwrap_or_default(),
        raw: zc.obj.raw.clone(),
        line: zc.obj.line,
        found: true,
        children: Vec::new(),
        specs: Vec::new(),
        group: None,
        stacked: false,
    };
    if let Some(el) = idx.find("ZoneHVAC:EquipmentList", &zc.equip_list) {
        collect_children(idx, el, &mut c.children);
    }
    c
}

fn air_demand_side(
    idx: &Index,
    loop_name: &str,
    inlet: &str,
    outlet: &str,
    warnings: &mut Vec<String>,
) -> Side {
    let mut side = Side {
        label: "Demand side".to_string(),
        inlet_node: inlet.to_string(),
        outlet_node: outlet.to_string(),
        ..Default::default()
    };
    let inlets = idx.resolve_nodes(inlet);

    // Splitter outlets from the supply path(s) feeding this loop's demand
    // inlet node(s). A path component is a ZoneSplitter or a SupplyPlenum.
    let mut outlets: Vec<String> = Vec::new();
    for path in idx.all("AirLoopHVAC:SupplyPath") {
        if !inlets.iter().any(|n| eq(n, path.field(1))) {
            continue;
        }
        let mut i = 2;
        while i + 1 < path.fields.len() {
            let (ctype, cname) = (path.field(i), path.field(i + 1));
            if let Some(o) = idx.find(ctype, cname) {
                let first_outlet = if ctype.to_ascii_lowercase().contains("plenum") {
                    4 // SupplyPlenum: name, zone, zone node, inlet, outlets...
                } else {
                    2 // ZoneSplitter: name, inlet, outlets...
                };
                outlets.extend(
                    o.fields[first_outlet.min(o.fields.len())..]
                        .iter()
                        .filter(|f| !f.is_empty())
                        .cloned(),
                );
                if side.splitter.is_none() {
                    side.splitter = Some(make_component(idx, ctype, cname, path.field(1), ""));
                }
            } else {
                warnings.push(format!(
                    "{loop_name} (demand): supply path component {ctype} \"{cname}\" not found"
                ));
            }
            i += 2;
        }
    }

    // Zone connections and air distribution units, for node matching.
    let zones: Vec<ZoneConn> = idx
        .all("ZoneHVAC:EquipmentConnections")
        .map(|o| ZoneConn {
            obj: o,
            zone: o.field(0).to_string(),
            inlets: idx.resolve_nodes(o.field(2)),
            returns: idx.resolve_nodes(o.field(5)),
            equip_list: o.field(1).to_string(),
        })
        .collect();
    let adus: Vec<&IdfObject> = idx.all("ZoneHVAC:AirDistributionUnit").collect();

    for node in &outlets {
        // Direct connection: the splitter outlet is a zone inlet node.
        if let Some(zc) = zones.iter().find(|z| z.inlets.iter().any(|n| eq(n, node))) {
            side.parallel.push(BranchView {
                name: zc.zone.clone(),
                components: vec![zone_component(idx, zc, node)],
            });
            continue;
        }
        // Through a terminal unit: ADU whose terminal references this node.
        let hit = adus.iter().find_map(|adu| {
            let (ttype, tname) = (adu.field(2), adu.field(3));
            let term = idx.find(ttype, tname)?;
            term.fields
                .iter()
                .any(|f| eq(f, node))
                .then_some((adu, term))
        });
        if let Some((adu, term)) = hit {
            let adu_outlet = adu.field(1);
            let mut comps = vec![make_component(
                idx,
                &term.class,
                term.field(0),
                node,
                adu_outlet,
            )];
            let zc = zones
                .iter()
                .find(|z| z.inlets.iter().any(|n| eq(n, adu_outlet)));
            let name = match zc {
                Some(zc) => {
                    comps.push(zone_component(idx, zc, adu_outlet));
                    zc.zone.clone()
                }
                None => {
                    warnings.push(format!(
                        "{loop_name} (demand): no zone found for terminal \"{}\"",
                        term.field(0)
                    ));
                    term.field(0).to_string()
                }
            };
            side.parallel.push(BranchView {
                name,
                components: comps,
            });
        } else {
            warnings.push(format!(
                "{loop_name} (demand): no zone or terminal found for supply path outlet \"{node}\""
            ));
            side.parallel.push(BranchView {
                name: node.clone(),
                components: vec![Component {
                    class: "?".to_string(),
                    name: node.clone(),
                    inlet: node.clone(),
                    outlet: String::new(),
                    ..Default::default()
                }],
            });
        }
    }
    if side.parallel.is_empty() {
        warnings.push(format!("{loop_name} (demand): no supply path found"));
    }

    // Mixer from the return path ending at the demand outlet node.
    for path in idx.all("AirLoopHVAC:ReturnPath") {
        if !eq(path.field(1), outlet) {
            continue;
        }
        let (ctype, cname) = (path.field(2), path.field(3));
        if idx.find(ctype, cname).is_some() {
            side.mixer = Some(make_component(idx, ctype, cname, "", outlet));
        }
        break;
    }
    side
}

// --- entry point ------------------------------------------------------------

pub fn build(objects: &[IdfObject]) -> Vec<HvacLoop> {
    let idx = Index::new(objects);
    let mut loops = Vec::new();

    for (class, kind) in [
        ("PlantLoop", LoopKind::Plant),
        ("CondenserLoop", LoopKind::Condenser),
    ] {
        for o in idx.all(class) {
            let name = o.field(0).to_string();
            let mut warnings = Vec::new();
            let sides = vec![
                build_side(
                    &idx,
                    &name,
                    "Supply side",
                    o.field(10),
                    o.field(11),
                    o.field(12),
                    o.field(13),
                    &mut warnings,
                ),
                build_side(
                    &idx,
                    &name,
                    "Demand side",
                    o.field(14),
                    o.field(15),
                    o.field(16),
                    o.field(17),
                    &mut warnings,
                ),
            ];
            loops.push(HvacLoop {
                kind,
                name,
                sides,
                raw: o.raw.clone(),
                line: o.line,
                warnings,
            });
        }
    }

    for o in idx.all("AirLoopHVAC") {
        let name = o.field(0).to_string();
        let mut warnings = Vec::new();
        let supply_outlet = idx
            .resolve_nodes(o.field(9))
            .into_iter()
            .next()
            .unwrap_or_default();
        let supply = build_side(
            &idx,
            &name,
            "Supply side",
            o.field(6),
            &supply_outlet,
            o.field(4),
            o.field(5),
            &mut warnings,
        );
        let demand = air_demand_side(&idx, &name, o.field(8), o.field(7), &mut warnings);
        loops.push(HvacLoop {
            kind: LoopKind::Air,
            name,
            sides: vec![supply, demand],
            raw: o.raw.clone(),
            line: o.line,
            warnings,
        });
    }

    loops.sort_by(|a, b| {
        (a.kind.label(), a.name.to_ascii_lowercase())
            .cmp(&(b.kind.label(), b.name.to_ascii_lowercase()))
    });
    loops
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::idf;

    const CHW: &str = "\
PlantLoop,
  CHW Loop, Water, , CHW Ops, CHW Supply Outlet, 98, 1, autosize, 0, autosize,
  CHW Supply Inlet, CHW Supply Outlet, CHW Supply Branches, CHW Supply Connectors,
  CHW Demand Inlet, CHW Demand Outlet, CHW Demand Branches, CHW Demand Connectors;

BranchList, CHW Supply Branches,
  CHW Pump Branch, Chiller Branch, CHW Supply Bypass, CHW Supply Outlet Branch;
ConnectorList, CHW Supply Connectors,
  Connector:Splitter, CHW Supply Splitter, Connector:Mixer, CHW Supply Mixer;
Connector:Splitter, CHW Supply Splitter, CHW Pump Branch, Chiller Branch, CHW Supply Bypass;
Connector:Mixer, CHW Supply Mixer, CHW Supply Outlet Branch, Chiller Branch, CHW Supply Bypass;

Branch, CHW Pump Branch, ,
  Pump:ConstantSpeed, CHW Pump, CHW Supply Inlet, CHW Pump Outlet;
Branch, Chiller Branch, ,
  Chiller:Electric:EIR, Chiller 1, Chiller Inlet, Chiller Outlet;
Branch, CHW Supply Bypass, ,
  Pipe:Adiabatic, CHW Supply Bypass Pipe, Bypass Inlet, Bypass Outlet;
Branch, CHW Supply Outlet Branch, ,
  Pipe:Adiabatic, CHW Supply Outlet Pipe, Mixer Outlet, CHW Supply Outlet;

BranchList, CHW Demand Branches, CHW Demand Inlet Branch, Coil Branch, CHW Demand Outlet Branch;
ConnectorList, CHW Demand Connectors,
  Connector:Splitter, CHW Demand Splitter, Connector:Mixer, CHW Demand Mixer;
Connector:Splitter, CHW Demand Splitter, CHW Demand Inlet Branch, Coil Branch;
Connector:Mixer, CHW Demand Mixer, CHW Demand Outlet Branch, Coil Branch;
Branch, CHW Demand Inlet Branch, ,
  Pipe:Adiabatic, CHW Demand Inlet Pipe, CHW Demand Inlet, Demand Split Inlet;
Branch, Coil Branch, ,
  Coil:Cooling:Water, CC-1, Coil Water Inlet, Coil Water Outlet;
Branch, CHW Demand Outlet Branch, ,
  Pipe:Adiabatic, CHW Demand Outlet Pipe, Demand Mix Outlet, CHW Demand Outlet;

Pump:ConstantSpeed, CHW Pump, CHW Supply Inlet, CHW Pump Outlet, 0.06309, 179344;
Chiller:Electric:EIR, Chiller 1, 1758425, 5.5, 6.67, 29.4, 0.06309, autosize;
Pipe:Adiabatic, CHW Supply Bypass Pipe, Bypass Inlet, Bypass Outlet;
Pipe:Adiabatic, CHW Supply Outlet Pipe, Mixer Outlet, CHW Supply Outlet;
Pipe:Adiabatic, CHW Demand Inlet Pipe, CHW Demand Inlet, Demand Split Inlet;
Pipe:Adiabatic, CHW Demand Outlet Pipe, Demand Mix Outlet, CHW Demand Outlet;
Coil:Cooling:Water, CC-1;
";

    #[test]
    fn plant_loop_topology() {
        let loops = build(&idf::parse(CHW));
        assert_eq!(loops.len(), 1);
        let l = &loops[0];
        assert_eq!(l.kind, LoopKind::Plant);
        assert_eq!(l.name, "CHW Loop");
        assert_eq!(l.sides.len(), 2);

        let supply = &l.sides[0];
        assert_eq!(supply.inlet_node, "CHW Supply Inlet");
        assert_eq!(supply.series_in.len(), 1);
        let pump = &supply.series_in[0].components[0];
        assert_eq!(pump.class, "Pump:ConstantSpeed");
        assert_eq!(pump.specs[0], ("Flow", "1,000 GPM".to_string()));
        assert_eq!(pump.specs[1], ("Head", "60 ft".to_string()));
        assert_eq!(supply.parallel.len(), 2);
        let chiller = &supply.parallel[0].components[0];
        assert_eq!(chiller.name, "Chiller 1");
        assert_eq!(chiller.specs[0], ("Capacity", "500 tons".to_string()));
        assert_eq!(chiller.specs[1], ("CHW flow", "1,000 GPM".to_string()));
        assert_eq!(supply.series_out.len(), 1);
        assert!(supply.splitter.is_some());
        assert!(supply.mixer.is_some());

        let demand = &l.sides[1];
        assert_eq!(demand.parallel.len(), 1);
        assert_eq!(demand.parallel[0].components[0].name, "CC-1");
    }

    const AIR: &str = "\
AirLoopHVAC,
  Sys 1, , , autosize, Sys 1 Branches, ,
  Sys 1 Air Loop Inlet, Sys 1 Return Outlet, Sys 1 Supply Path Inlet, Sys 1 Fan Outlet;
BranchList, Sys 1 Branches, Sys 1 Main Branch;
Branch, Sys 1 Main Branch, ,
  AirLoopHVAC:UnitarySystem, Sys 1 Unitary, Sys 1 Air Loop Inlet, Sys 1 Fan Outlet;
AirLoopHVAC:UnitarySystem,
  Sys 1 Unitary, Load, Zone 1, , , Sys 1 Air Loop Inlet, Sys 1 Fan Outlet,
  Fan:OnOff, Sys 1 Fan, BlowThrough, , Coil:Heating:Fuel, Sys 1 HC, ,
  Coil:Cooling:DX, Sys 1 CC;
Fan:OnOff, Sys 1 Fan, , 0.6, 622.72, 4.7195, 0.9;
Coil:Heating:Fuel, Sys 1 HC;
Coil:Cooling:DX, Sys 1 CC, CC Inlet, CC Outlet, , , Cond In, Cond Out, Sys 1 CC Perf;
Coil:Cooling:DX:CurveFit:Performance,
  Sys 1 CC Perf, 0, , -25, 10, , Discrete, 0, 2, , Electricity, Sys 1 CC Mode;
Coil:Cooling:DX:CurveFit:OperatingMode,
  Sys 1 CC Mode, 70337, autosize, autosize, 0, 0, 0, 0, No, AirCooled, 0, 2,
  Sys 1 CC Speed 1, Sys 1 CC Speed 2;
Coil:Cooling:DX:CurveFit:Speed, Sys 1 CC Speed 1, 0.5, 0.5, 0.5, autosize, 4.5;
Coil:Cooling:DX:CurveFit:Speed, Sys 1 CC Speed 2, 1.0, 1.0, 1.0, autosize, 3.8;

AirLoopHVAC:SupplyPath, Sys 1 Supply Path, Sys 1 Supply Path Inlet,
  AirLoopHVAC:ZoneSplitter, Sys 1 Splitter;
AirLoopHVAC:ZoneSplitter, Sys 1 Splitter, Sys 1 Supply Path Inlet, Zone 1 Equip Inlet;
AirLoopHVAC:ReturnPath, Sys 1 Return Path, Sys 1 Return Outlet,
  AirLoopHVAC:ZoneMixer, Sys 1 Mixer;
AirLoopHVAC:ZoneMixer, Sys 1 Mixer, Sys 1 Return Outlet, Zone 1 Return;

ZoneHVAC:EquipmentConnections,
  Zone 1, Zone 1 Equipment, Zone 1 Supply Inlet, , Zone 1 Air Node, Zone 1 Return;
ZoneHVAC:EquipmentList, Zone 1 Equipment, SequentialLoad,
  ZoneHVAC:AirDistributionUnit, Zone 1 ADU, 1, 1;
ZoneHVAC:AirDistributionUnit, Zone 1 ADU, Zone 1 Supply Inlet,
  AirTerminal:SingleDuct:ConstantVolume:NoReheat, Zone 1 CV;
AirTerminal:SingleDuct:ConstantVolume:NoReheat,
  Zone 1 CV, , Zone 1 Equip Inlet, Zone 1 Supply Inlet, autosize;
";

    #[test]
    fn air_loop_topology() {
        let loops = build(&idf::parse(AIR));
        assert_eq!(loops.len(), 1);
        let l = &loops[0];
        assert_eq!(l.kind, LoopKind::Air);
        assert!(l.warnings.is_empty(), "{:?}", l.warnings);

        let supply = &l.sides[0];
        assert_eq!(supply.series_in.len(), 1);
        // The unitary system is expanded into its fan/coil chain: blow-through
        // fan first, then cooling coil, then heating coil, all grouped under
        // the unitary.
        let comps = &supply.series_in[0].components;
        assert_eq!(comps.len(), 3);
        let fan = &comps[0];
        assert_eq!(fan.class, "Fan:OnOff");
        assert_eq!(fan.group.as_deref(), Some("Sys 1 Unitary"));
        assert_eq!(fan.inlet, "Sys 1 Air Loop Inlet");
        assert_eq!(
            fan.specs,
            vec![
                ("Flow", "10,000 CFM".to_string()),
                ("ΔP", "2.5 in. w.c.".to_string()),
                ("Power", "4.9 kW (6.57 hp)".to_string()),
                ("Motor eff", "90%".to_string()),
                ("Fan eff", "60%".to_string()),
            ]
        );
        // Back-reference to the unitary parent for the details panel.
        assert!(fan.children.iter().any(|c| c.name == "Sys 1 Unitary"));
        // New-style DX coil: capacity from the operating mode, COP from the
        // nominal (2nd) speed.
        let cc = &comps[1];
        assert_eq!(cc.class, "Coil:Cooling:DX");
        assert_eq!(
            cc.specs,
            vec![
                ("Capacity", "20 tons (240 kBtu/h)".to_string()),
                ("COP", "3.8".to_string()),
            ]
        );
        let hc = &comps[2];
        assert_eq!(hc.class, "Coil:Heating:Fuel");
        assert_eq!(hc.outlet, "Sys 1 Fan Outlet");

        let demand = &l.sides[1];
        assert!(demand.splitter.is_some());
        assert!(demand.mixer.is_some());
        assert_eq!(demand.parallel.len(), 1);
        let row = &demand.parallel[0];
        assert_eq!(row.name, "Zone 1");
        assert_eq!(row.components.len(), 2);
        assert_eq!(
            row.components[0].class,
            "AirTerminal:SingleDuct:ConstantVolume:NoReheat"
        );
        assert_eq!(row.components[1].class, "Zone");
        assert_eq!(row.components[1].outlet, "Zone 1 Return");
    }

    // A 100%-OA unit with a preheat coil and energy wheel on the intake, an
    // exhaust fan on the relief side, and the supply fan on the main branch.
    // The equipment list is deliberately out of flow order (wheel before
    // preheat) to exercise the node-based ordering.
    const OA: &str = "\
AirLoopHVAC,
  DOAS, , , 2.792, DOAS Branches, ,
  DOAS Air Loop Inlet, DOAS Return Outlet, DOAS Supply Path Inlet, DOAS Unit Outlet;
BranchList, DOAS Branches, DOAS Main Branch;
Branch, DOAS Main Branch, ,
  AirLoopHVAC:OutdoorAirSystem, DOAS OA System, DOAS Air Loop Inlet, DOAS Mixed Air Outlet,
  Fan:SystemModel, DOAS Supply Fan, DOAS Mixed Air Outlet, DOAS Unit Outlet;
AirLoopHVAC:OutdoorAirSystem, DOAS OA System, DOAS OA Controllers, DOAS OA Equipment;
AirLoopHVAC:ControllerList, DOAS OA Controllers, Controller:OutdoorAir, DOAS OA Controller;
Controller:OutdoorAir, DOAS OA Controller, DOAS Relief Air Outlet, DOAS Air Loop Inlet,
  DOAS Mixed Air Outlet, DOAS Outdoor Air Inlet, 2.792, 2.792;
AirLoopHVAC:OutdoorAirSystem:EquipmentList, DOAS OA Equipment,
  HeatExchanger:AirToAir:SensibleAndLatent, DOAS Energy Wheel,
  Coil:Heating:Electric, DOAS Preheat Coil,
  OutdoorAir:Mixer, DOAS OA Mixer,
  Fan:SystemModel, DOAS Exhaust Fan;
OutdoorAir:Mixer, DOAS OA Mixer,
  DOAS Mixed Air Outlet, DOAS HX Supply Outlet, DOAS Relief Air Outlet, DOAS Air Loop Inlet;
Coil:Heating:Electric, DOAS Preheat Coil, , 1.0, 17973,
  DOAS Outdoor Air Inlet, DOAS Preheat Outlet, DOAS Preheat Outlet;
HeatExchanger:AirToAir:SensibleAndLatent, DOAS Energy Wheel, , 2.792,
  0.754, 0.717, 0.749, 0.711,
  DOAS Preheat Outlet, DOAS HX Supply Outlet, DOAS Relief Air Outlet, DOAS HX Exhaust Outlet;
Fan:SystemModel, DOAS Exhaust Fan, , DOAS HX Exhaust Outlet, DOAS Exhaust Fan Outlet, 2.029;
Fan:SystemModel, DOAS Supply Fan, , DOAS Mixed Air Outlet, DOAS Unit Outlet, 2.792;

AirLoopHVAC:SupplyPath, DOAS Supply Path, DOAS Supply Path Inlet,
  AirLoopHVAC:ZoneSplitter, DOAS Splitter;
AirLoopHVAC:ZoneSplitter, DOAS Splitter, DOAS Supply Path Inlet, Zone 1 Supply Inlet;
AirLoopHVAC:ReturnPath, DOAS Return Path, DOAS Return Outlet,
  AirLoopHVAC:ZoneMixer, DOAS Return Mixer;
AirLoopHVAC:ZoneMixer, DOAS Return Mixer, DOAS Return Outlet, Zone 1 Return;
ZoneHVAC:EquipmentConnections,
  Zone 1, Zone 1 Equipment, Zone 1 Supply Inlet, , Zone 1 Air Node, Zone 1 Return;
ZoneHVAC:EquipmentList, Zone 1 Equipment, SequentialLoad;
";

    #[test]
    fn oa_system_expands_into_equipment() {
        let loops = build(&idf::parse(OA));
        assert_eq!(loops.len(), 1);
        let l = &loops[0];
        assert!(l.warnings.is_empty(), "{:?}", l.warnings);

        let supply = &l.sides[0];
        assert_eq!(supply.series_in.len(), 1);
        let comps = &supply.series_in[0].components;
        let classes: Vec<&str> = comps.iter().map(|c| c.class.as_str()).collect();
        // Intake chain in flow order despite the scrambled equipment list,
        // then the mixer, then the branch's own fan. The relief-side exhaust
        // fan is NOT on the supply line.
        assert_eq!(
            classes,
            vec![
                "Coil:Heating:Electric",
                "HeatExchanger:AirToAir:SensibleAndLatent",
                "OutdoorAir:Mixer",
                "Fan:SystemModel",
            ]
        );

        // Real nodes from the IDF, unlike the unitary's unnamed interiors.
        let preheat = &comps[0];
        assert_eq!(preheat.inlet, "DOAS Outdoor Air Inlet");
        assert_eq!(preheat.outlet, "DOAS Preheat Outlet");
        assert_eq!(preheat.group.as_deref(), Some("DOAS OA System"));
        assert_eq!(preheat.specs, vec![("Capacity", "61.3 kBtu/h".to_string())]);
        assert!(preheat.children.iter().any(|c| c.name == "DOAS OA System"));

        let wheel = &comps[1];
        assert_eq!(
            wheel.specs,
            vec![
                ("Flow", "5,916 CFM".to_string()),
                ("Sens eff", "75.4%".to_string()),
                ("Lat eff", "71.7%".to_string()),
            ]
        );

        // The mixer carries the branch connections (return air in, mixed air
        // out) and surfaces the OA controller.
        let mixer = &comps[2];
        assert_eq!(mixer.inlet, "DOAS Air Loop Inlet");
        assert_eq!(mixer.outlet, "DOAS Mixed Air Outlet");
        assert_eq!(mixer.group.as_deref(), Some("DOAS OA System"));
        assert!(
            mixer
                .children
                .iter()
                .any(|c| eq(&c.class, "Controller:OutdoorAir"))
        );

        // The supply fan is a plain branch component outside the group.
        assert_eq!(comps[3].group, None);

        // The relief stream is its own run: mixer relief node → heat
        // exchanger exhaust side (its other two nodes) → exhaust fan.
        assert_eq!(supply.aux.len(), 1);
        let relief = &supply.aux[0];
        assert_eq!(relief.name, "DOAS OA System relief");
        let rc: Vec<(&str, &str, &str)> = relief
            .components
            .iter()
            .map(|c| (c.name.as_str(), c.inlet.as_str(), c.outlet.as_str()))
            .collect();
        assert_eq!(
            rc,
            vec![
                (
                    "DOAS Energy Wheel",
                    "DOAS Relief Air Outlet",
                    "DOAS HX Exhaust Outlet"
                ),
                (
                    "DOAS Exhaust Fan",
                    "DOAS HX Exhaust Outlet",
                    "DOAS Exhaust Fan Outlet"
                ),
            ]
        );
        assert_eq!(
            relief.components[0].group.as_deref(),
            Some("DOAS OA System")
        );
    }

    // A service hot water loop: water heater on the supply side, fixtures
    // behind a WaterUse:Connections on the demand side.
    const DHW: &str = "\
PlantLoop,
  DHW Loop, Water, , DHW Ops, DHW Supply Outlet, 82.22, 5, autosize, 0, autocalculate,
  DHW Supply Inlet, DHW Supply Outlet, DHW Supply Branches, ,
  DHW Demand Inlet, DHW Demand Outlet, DHW Demand Branches, ;

BranchList, DHW Supply Branches, DHW Heater Branch;
Branch, DHW Heater Branch, ,
  WaterHeater:Mixed, DHW Heater, DHW Supply Inlet, DHW Supply Outlet;
WaterHeater:Mixed,
  DHW Heater, 0.151416, DHW Supply Setpoint, 1.0, 82.22, Modulate, 58614;

BranchList, DHW Demand Branches, DHW Use Branch;
Branch, DHW Use Branch, ,
  WaterUse:Connections, DHW Use Connections, DHW Demand Inlet, DHW Demand Outlet;
WaterUse:Connections,
  DHW Use Connections, DHW Demand Inlet, DHW Demand Outlet, , , , , None, Plant, ,
  DHW Sink Use, DHW Shower Use;
WaterUse:Equipment,
  DHW Sink Use, DHW Sinks, 0.00006309, Sink Fractions, DHW Sink Target Temperature;
WaterUse:Equipment,
  DHW Shower Use, DHW Showers, 0.00164, Shower Fractions, DHW Shower Target Temperature;

Schedule:Constant, DHW Supply Setpoint, Any Number, 60;
Schedule:Constant, DHW Sink Target Temperature, Any Number, 43.33;
Schedule:Constant, DHW Shower Target Temperature, Any Number, 37.78;
";

    #[test]
    fn dhw_loop_water_use_expansion() {
        let loops = build(&idf::parse(DHW));
        assert_eq!(loops.len(), 1);
        let l = &loops[0];
        assert_eq!(l.kind, LoopKind::Plant);
        assert!(l.warnings.is_empty(), "{:?}", l.warnings);

        // The water heater box shows tank, capacity, and the setpoint
        // resolved from its Schedule:Constant, in °F.
        let heater = &l.sides[0].series_in[0].components[0];
        assert_eq!(heater.class, "WaterHeater:Mixed");
        assert_eq!(
            heater.specs,
            vec![
                ("Tank", "40 gal".to_string()),
                ("Heater", "200 kBtu/h".to_string()),
                ("Setpoint", "140°F".to_string()),
            ]
        );

        // The connections object is expanded into its fixtures, grouped
        // under it and marked as a parallel stack; every fixture is a tap
        // across the branch inlet/outlet.
        let comps = &l.sides[1].series_in[0].components;
        assert_eq!(comps.len(), 2);
        let sink = &comps[0];
        assert_eq!(sink.class, "WaterUse:Equipment");
        assert_eq!(sink.name, "DHW Sink Use");
        assert_eq!(sink.inlet, "DHW Demand Inlet");
        assert_eq!(sink.outlet, "DHW Demand Outlet");
        assert!(sink.stacked);
        assert_eq!(sink.group.as_deref(), Some("DHW Use Connections"));
        assert_eq!(
            sink.specs,
            vec![
                ("Peak flow", "1 GPM".to_string()),
                ("Target temp", "110°F".to_string()),
            ]
        );
        assert!(sink.children.iter().any(|c| c.name == "DHW Use Connections"));
        let shower = &comps[1];
        assert_eq!(shower.name, "DHW Shower Use");
        assert_eq!(shower.inlet, "DHW Demand Inlet");
        assert_eq!(shower.outlet, "DHW Demand Outlet");
        assert!(shower.stacked);
        assert_eq!(
            shower.specs,
            vec![
                ("Peak flow", "26 GPM".to_string()),
                ("Target temp", "100°F".to_string()),
            ]
        );
    }
}
