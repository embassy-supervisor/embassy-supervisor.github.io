// Preset scenarios: DSL text (fully editable), behavior bindings for the
// generic workers, and the device widgets the right rail shows.

/** What a momentary `button` widget fires. */
export type ButtonAction =
  | { type: 'power'; cmd: 'sleep' | 'wake' }
  | { type: 'node'; node: string; op: 'start' | 'stop' | 'resume' }
  | { type: 'resource'; resource: string; cmd: 'provide' | 'clear' }
  /** A one-tick input pulse: value, then back to the widget's resting 0. */
  | { type: 'pulse'; node: string; value: number };

export interface DeviceSpec {
  kind: 'slider' | 'switch' | 'dial' | 'lease' | 'button' | 'gauge';
  /** Node name (set_input target); signal name for `lease` and value/depth
   *  gauges, a node for `grant` gauges, a resource for `granted`/`capacity`. */
  target: string;
  label: string;
  hint?: string;
  initial?: number;
  /** dial/slider range (defaults 0..1). */
  min?: number;
  max?: number;
  /** switch state texts (defaults 'up'/'down'). */
  onLabel?: string;
  offLabel?: string;
  /** button: what pressing it fires. */
  action?: ButtonAction;
  /** gauge: what it reads (default 'value'). `value`/`depth` read a signal,
   *  `grant` a node's budget share, `granted`/`capacity` a divisible resource. */
  source?: 'value' | 'depth' | 'grant' | 'granted' | 'capacity';
  unit?: string;
}

export interface Scenario {
  id: string;
  title: string;
  blurb: string;
  /** Toolbar optgroup: a researched system or a mechanism tour. */
  group: 'systems' | 'mechanisms';
  /** One line naming the supervisor mechanisms this scenario exercises. */
  mechanisms?: string;
  /** Plane grouping: cluster label -> item names (nodes/pools). */
  planes?: Record<string, string[]>;
  /** Declared core per executor name (single-threaded wasm: display only;
   * `trace::set_core_id_fn` is the on-hardware mechanism). Root is core 0. */
  cores?: Record<string, number>;
  dsl: string;
  behaviors: Record<string, unknown>;
  devices: DeviceSpec[];
}

export const scenarios: Scenario[] = [
  {
    id: 'asset-tracker',
    title: 'Cellular asset tracker',
    group: 'systems',
    mechanisms:
      'power coordinator (teardown / resume_pausable / respawn_terminate), Pause parking with kept state, consume resources failing closed, ready_on_write, detached self-test, disabled latch across sleep',
    blurb:
      'A battery-powered tracker where energy, not time, is the scarce resource. Sleep tears the graph down in reverse order: Pause nodes park keeping their state, the modem runner is consumed and gone. Wake without rebuilding it and the modem fails closed. Rebuild, wake again, and the graph comes back. GNSS is ready only while a fix is actually being produced.',
    planes: {
      Sensing: ['MOTION', 'TEMP', 'GNSS'],
      Store: ['BUFFER', 'GEOFENCE'],
      Radio: ['MODEM', 'UPLINK', 'FOTA'],
    },
    dsl: `supervisor_graph! {
    name: TRACKER;

    node POWER = Terminate;

    node MOTION = Pause, task: crate::sense::motion_task,
        writes: [signals::MOTION observed];

    node TEMP = Pause, task: crate::sense::temp_task,
        writes: [signals::TEMPERATURE observed];

    node GNSS = Terminate, task: crate::sense::gnss_task,
        ready_on_write, beat_timeout: 1500,
        writes: [signals::FIX observed beat];

    node BUFFER = Pause, deps: [MOTION],
        task: crate::store::buffer_task,
        reads: [signals::MOTION, signals::TEMPERATURE, signals::FIX],
        writes: [signals::RECORDS observed];

    node GEOFENCE = Terminate, deps: [GNSS ready],
        slot_timeout: 3000,
        task: crate::geo::fence_task,
        reads: [signals::FIX],
        writes: [signals::ALERTS observed];

    node MODEM = Terminate, task: crate::modem::runner_task,
        resources: [MODEM_HW: consume crate::modem::ModemHw],
        provides: [NET], slot_timeout: 3000;

    node UPLINK = Terminate, deps: [MODEM ready bound, BUFFER],
        task: crate::uplink::uplink_task,
        resources: [NET: shared embassy_net::Stack<'static>],
        slot_timeout: 3000,
        reads: [signals::RECORDS, signals::ALERTS];

    node FOTA = Terminate, deps: [MODEM ready],
        task: crate::fota::fota_task, disabled, slot_timeout: 3000;

    node SELFTEST = Terminate, deps: [UPLINK],
        task: crate::diag::selftest_task;
}`,
    behaviors: {
      POWER: { kind: 'power_coordinator' },
      MOTION: { kind: 'periodic', period_ms: 400, scaled: true },
      TEMP: { kind: 'periodic', period_ms: 1200 },
      GNSS: { kind: 'periodic', period_ms: 600 },
      BUFFER: { kind: 'queue', capacity: 16, policy: 'drop_oldest', drain_ms: 250 },
      GEOFENCE: { kind: 'control_loop', period_ms: 500 },
      MODEM: { kind: 'provider', startup_ms: 800 },
      UPLINK: { kind: 'pipeline', work_ms: 400 },
      FOTA: { kind: 'oneshot', run_ms: 2500 },
      SELFTEST: { kind: 'selftest', run_ms: 700 },
    },
    devices: [
      {
        kind: 'button',
        target: 'POWER',
        label: 'Sleep',
        action: { type: 'power', cmd: 'sleep' },
        hint: 'reverse-order teardown: Pause nodes park, the consumed modem runner is gone',
      },
      {
        kind: 'button',
        target: 'POWER',
        label: 'Wake',
        action: { type: 'power', cmd: 'wake' },
        hint: 'parked nodes resume in place; Terminate nodes respawn; the modem fails closed until rebuilt',
      },
      {
        kind: 'button',
        target: 'MODEM_HW',
        label: 'Rebuild modem runner',
        action: { type: 'resource', resource: 'MODEM_HW', cmd: 'provide' },
        hint: 're-provide the consumed slot, then Wake again',
      },
      {
        kind: 'slider',
        target: 'MOTION',
        label: 'Motion',
        initial: 0.5,
        hint: 'movement feeds the record FIFO: a still asset logs only fixes',
      },
      {
        kind: 'gauge',
        target: 'signals::RECORDS',
        label: 'Record FIFO',
        source: 'depth',
        max: 16,
        hint: 'kept across the sleep: a parked Pause node holds its state',
      },
    ],
  },
  {
    id: 'edge-gateway',
    title: 'Industrial edge gateway',
    group: 'systems',
    mechanisms:
      'Pause parking, shared resources, elastic poll pool on a serialized bus, floor-member pool deps, queue overflow (drop_oldest), ready bound uplink, watchdog',
    blurb:
      'Southbound Modbus pollers share one half-duplex RS-485 bus, a tag database and deadband filter sit above them, and a single Sparkplug session runs northbound behind a store-and-forward spool. Drop the uplink and the backlog climbs while polling continues; restore it and the backlog drains. Add devices and the poll pool grows; remove them and it folds back.',
    planes: {
      Fieldbus: ['RS485', 'FIELD_POLL'],
      Processing: ['TAG_DB', 'DEADBAND'],
      Uplink: ['SPOOL', 'SPARKPLUG', 'MODEM', 'HTTP_API', 'OTA'],
    },
    cores: { FIELDBUS: 1 },
    dsl: `supervisor_graph! {
    executor FIELDBUS;

    node WATCHDOG = Terminate,
        task: crate::watchdog::feed_task;

    node RS485 = Pause, executor: FIELDBUS,
        task: crate::fieldbus::bus_task,
        provides: [BUS485], slot_timeout: 2000;

    pool FIELD_POLL = [Terminate, OnDemand, OnDemand, OnDemand],
        executor: FIELDBUS, deps: [RS485 ready],
        task: crate::fieldbus::poll_task,
        policy: DeferredShrink::new(Duration::from_secs(3)),
        min: 1, max: 4, slot_timeout: 2000,
        resources: [BUS485: shared crate::fieldbus::Rs485Port],
        writes: [signals::TAGS observed];

    node TAG_DB = Terminate, deps: [FIELD_POLL],
        task: crate::tags::db_task,
        reads: [signals::TAGS],
        writes: [signals::CHANGES observed];

    node DEADBAND = Terminate, deps: [TAG_DB],
        task: crate::tags::deadband_task,
        reads: [signals::CHANGES],
        writes: [signals::EVENTS observed];

    node SPOOL = Terminate, deps: [DEADBAND],
        task: crate::uplink::spool_task,
        reads: [signals::EVENTS],
        writes: [signals::BATCH observed];

    node MODEM = Terminate, task: crate::uplink::modem_task,
        provides: [NET_STACK], slot_timeout: 8000;

    node SPARKPLUG = Terminate, deps: [MODEM ready bound, SPOOL],
        task: crate::uplink::sparkplug_task,
        resources: [NET_STACK: shared embassy_net::Stack<'static>],
        slot_timeout: 5000,
        reads: [signals::BATCH];

    node HTTP_API = Terminate, deps: [MODEM ready],
        task: crate::http::api_task,
        resources: [NET_STACK: shared embassy_net::Stack<'static>],
        slot_timeout: 5000;

    node OTA = Terminate, deps: [MODEM ready],
        task: crate::ota::ota_task, disabled, slot_timeout: 5000;
}`,
    behaviors: {
      WATCHDOG: { kind: 'watchdog', feed_ms: 500 },
      RS485: { kind: 'provider', startup_ms: 300 },
      FIELD_POLL: { kind: 'poller', period_ms: 400, txn_ms: 40 },
      TAG_DB: { kind: 'pipeline', work_ms: 200 },
      DEADBAND: { kind: 'pipeline', work_ms: 250 },
      SPOOL: { kind: 'queue', capacity: 12, policy: 'drop_oldest', drain_ms: 300 },
      MODEM: { kind: 'link', initially_up: true },
      SPARKPLUG: { kind: 'pipeline', work_ms: 300 },
      HTTP_API: { kind: 'idle' },
      OTA: { kind: 'oneshot', run_ms: 2500 },
    },
    devices: [
      {
        kind: 'switch',
        target: 'MODEM',
        label: 'Uplink',
        hint: 'the bound Sparkplug session stops when it drops; the spool absorbs the backlog',
        initial: 1,
      },
      {
        kind: 'dial',
        target: 'FIELD_POLL',
        label: 'Attached devices',
        hint: 'more device groups stretch each poll transaction; busy pollers grow the pool',
        min: 0,
        max: 4,
        initial: 0,
      },
      {
        kind: 'gauge',
        target: 'signals::BATCH',
        label: 'Spool backlog',
        source: 'depth',
        max: 12,
        hint: 'store-and-forward depth: climbs while the uplink is down, drains when it returns',
      },
    ],
  },
  {
    id: 'bms',
    title: 'Battery management system',
    group: 'systems',
    mechanisms:
      'safety executor tier, beat_window liveness, app-owned escalation (clear_ready opens the contactors; activate:LIMP), precharge as a gated provider, min:0 balancing pool, ready bound',
    blurb:
      'The safety ladder certified BMS firmware runs: protection on its own executor tier, a precharge that must reach threshold before the contactors may close, and an escalation policy that answers a stalled protection loop by withdrawing its readiness; the contactors open through the ready bound edge, the safe state. Stall the SoC estimator instead and the policy activates the LIMP limiter.',
    planes: {
      Safety: ['CURRENT', 'VOLTAGE', 'PROTECT'],
      'High voltage': ['PRECHARGE', 'CONTACTORS', 'CHARGER'],
      Application: ['TEMP', 'BALANCE', 'SOC', 'LIMP', 'DISPLAY'],
    },
    cores: { SAFETY: 1 },
    dsl: `supervisor_graph! {
    executor SAFETY;

    node CURRENT = Terminate, executor: SAFETY,
        task: crate::sense::current_task,
        beat_timeout: 200, beat_window: 2,
        writes: [signals::PACK_CURRENT observed beat];

    node VOLTAGE = Terminate, executor: SAFETY,
        task: crate::sense::cell_volt_task,
        beat_timeout: 200, beat_window: 2,
        writes: [signals::CELL_VOLTS observed beat];

    node TEMP = Terminate, task: crate::sense::temp_task,
        writes: [signals::TEMPS observed];

    node PROTECT = Terminate, executor: SAFETY,
        deps: [CURRENT, VOLTAGE, TEMP],
        task: crate::protect::protect_task,
        beat_timeout: 200, beat_window: 2,
        reads: [signals::PACK_CURRENT, signals::CELL_VOLTS, signals::TEMPS],
        writes: [signals::LIMITS observed beat];

    node PRECHARGE = Terminate, task: crate::hv::precharge_task,
        provides: [HV_BUS], slot_timeout: 1500;

    node CONTACTORS = Terminate,
        deps: [PRECHARGE ready, PROTECT ready bound],
        task: crate::hv::contactor_task,
        resources: [HV_BUS: shared crate::hv::HvBus],
        slot_timeout: 3000,
        reads: [signals::LIMITS];

    pool BALANCE = [OnDemand, OnDemand, OnDemand, OnDemand],
        deps: [VOLTAGE], task: crate::balance::bleed_task,
        policy: DeferredShrink::new(Duration::from_secs(2)),
        min: 0, max: 4,
        reads: [signals::CELL_VOLTS];

    node SOC = Terminate, deps: [CURRENT, VOLTAGE],
        task: crate::soc::estimate_task,
        beat_timeout: 400,
        reads: [signals::PACK_CURRENT, signals::CELL_VOLTS],
        writes: [signals::SOC observed beat];

    node LIMP = Terminate, disabled,
        task: crate::soc::limp_task,
        writes: [signals::LIMITS];

    node CHARGER = Terminate, deps: [CONTACTORS],
        task: crate::charge::charger_task,
        reads: [signals::LIMITS];

    node DISPLAY = Terminate, task: crate::ui::display_task,
        reads: [signals::SOC];
}`,
    behaviors: {
      nodes: {
        CURRENT: { kind: 'periodic', period_ms: 100 },
        VOLTAGE: { kind: 'periodic', period_ms: 100 },
        TEMP: { kind: 'periodic', period_ms: 1000 },
        PROTECT: { kind: 'control_loop', period_ms: 100 },
        PRECHARGE: { kind: 'provider', startup_ms: 800 },
        CONTACTORS: { kind: 'control_loop', period_ms: 200 },
        BALANCE: { kind: 'session', busy_ms: 400 },
        SOC: { kind: 'control_loop', period_ms: 200 },
        LIMP: { kind: 'periodic', period_ms: 300 },
        CHARGER: { kind: 'control_loop', period_ms: 300 },
        DISPLAY: { kind: 'pipeline', work_ms: 400 },
      },
      escalation: {
        PROTECT: 'clear_ready',
        SOC: 'activate:LIMP',
      },
    },
    devices: [
      {
        kind: 'slider',
        target: 'CURRENT',
        label: 'Pack current',
        initial: 0.4,
        hint: 'what the safety tier samples at 10 Hz',
      },
      {
        kind: 'dial',
        target: 'BALANCE',
        label: 'Cells out of balance',
        min: 0,
        max: 4,
        initial: 0,
        hint: 'each unbalanced cell opens a bleed channel; the thermal budget caps them at 4',
      },
      {
        kind: 'gauge',
        target: 'signals::SOC',
        label: 'State of charge',
        source: 'value',
        hint: 'coulomb counting cross-checked against the cell-voltage sum',
      },
    ],
  },
  {
    id: 'energy-meter',
    title: 'Smart energy meter',
    group: 'systems',
    mechanisms:
      'metrology executor tier, ready_on_write metering, Pause profile log, min:0 session pool with a hard cap, ready bound uplink vs authoritative local record',
    blurb:
      'Metrology sampling on its own tier, tariff registers above, and the local load profile as the authoritative record. Client associations are a min:0 pool capped at the session limit, bound to the PLC carrier. Drop the carrier and the associations stop; the load profile keeps growing: the uplink is opportunistic, the record is not.',
    planes: {
      Metrology: ['SAMPLER', 'ACCUM'],
      Registers: ['CLOCK', 'TARIFF', 'PROFILE_LOG', 'NVM'],
      Comms: ['COMMS', 'DLMS'],
      Protection: ['TAMPER', 'DISCONNECT'],
    },
    cores: { METROLOGY: 1 },
    dsl: `supervisor_graph! {
    executor METROLOGY;

    node SAMPLER = Terminate, executor: METROLOGY,
        task: crate::metrology::sampler_task,
        ready_on_write, beat_timeout: 500,
        writes: [signals::WAVEFORM observed beat];

    node ACCUM = Terminate, executor: METROLOGY,
        deps: [SAMPLER ready], slot_timeout: 2000,
        task: crate::metrology::accumulate_task,
        reads: [signals::WAVEFORM],
        writes: [signals::ENERGY observed];

    node CLOCK = Terminate, task: crate::rtc::clock_task,
        writes: [signals::TIME observed];

    node TARIFF = Terminate, deps: [ACCUM, CLOCK],
        task: crate::registers::tariff_task,
        reads: [signals::ENERGY, signals::TIME],
        writes: [signals::REGISTERS observed];

    node PROFILE_LOG = Pause, deps: [TARIFF],
        task: crate::registers::profile_task,
        reads: [signals::REGISTERS],
        writes: [signals::PROFILE observed];

    node NVM = Terminate, deps: [PROFILE_LOG],
        task: crate::store::journal_task,
        reads: [signals::PROFILE];

    node COMMS = Terminate, task: crate::plc::carrier_task,
        provides: [PLC_LINK], slot_timeout: 5000;

    pool DLMS = [OnDemand, OnDemand, OnDemand],
        deps: [COMMS ready bound],
        task: crate::dlms::association_task,
        policy: DeferredShrink::new(Duration::from_secs(2)),
        min: 0, max: 3, slot_timeout: 2000,
        resources: [PLC_LINK: shared crate::plc::Plc],
        reads: [signals::REGISTERS];

    node TAMPER = Terminate, task: crate::protect::tamper_task,
        writes: [signals::TAMPER_EVT observed];

    node DISCONNECT = Terminate, deps: [TAMPER],
        task: crate::protect::disconnect_task,
        reads: [signals::TAMPER_EVT];

    node DISPLAY = Terminate, task: crate::ui::lcd_task,
        reads: [signals::REGISTERS];
}`,
    behaviors: {
      SAMPLER: { kind: 'periodic', period_ms: 100 },
      ACCUM: { kind: 'pipeline', work_ms: 250, accumulate: true },
      CLOCK: { kind: 'periodic', period_ms: 1000 },
      TARIFF: { kind: 'pipeline', work_ms: 300 },
      PROFILE_LOG: { kind: 'pipeline', work_ms: 600 },
      NVM: { kind: 'pipeline', work_ms: 800 },
      COMMS: { kind: 'link', initially_up: true },
      DLMS: { kind: 'session', busy_ms: 500 },
      TAMPER: { kind: 'periodic', period_ms: 4000 },
      DISCONNECT: { kind: 'control_loop', period_ms: 400 },
      DISPLAY: { kind: 'pipeline', work_ms: 500 },
    },
    devices: [
      {
        kind: 'switch',
        target: 'COMMS',
        label: 'PLC carrier',
        onLabel: 'carrier',
        offLabel: 'lost',
        initial: 1,
        hint: 'the association pool is bound to it; the load profile is not',
      },
      {
        kind: 'dial',
        target: 'DLMS',
        label: 'Client associations',
        min: 0,
        max: 3,
        initial: 0,
        hint: 'a real meter answers the fourth with BadTooManySessions: max is a hard cap',
      },
      {
        kind: 'gauge',
        target: 'signals::ENERGY',
        label: 'Energy accumulator',
        source: 'value',
        max: 600,
        unit: ' Wh',
        hint: 'metered on the metrology tier; the register only ever grows',
      },
    ],
  },
  {
    id: 'robot-cell',
    title: 'Robot cell controller',
    group: 'systems',
    mechanisms:
      'three executor planes, consume EtherCAT master with OP as a ready gate, all three overflow policies side by side, ready_on_write servos, a bound STO cascade through the safety plane',
    blurb:
      'Planning feeds a hard-RT motion tier through a back-pressured segment queue, telemetry leaves through a drop-oldest ring, and vision frames run on a reject pool: the three overflow policies side by side, each correct for its direction. Stall a safety channel and STO asserts as a bound cascade: the servos stop in order. Starve the segment queue and the motion tier holds last-good: a controlled stop, not a freeze.',
    planes: {
      Planning: ['PENDANT', 'INTERPRETER', 'PATH_PLANNER', 'IK_SOLVER', 'SEGMENT_Q', 'VISION', 'VISION_Q', 'LOGGER'],
      Motion: ['ECAT_MASTER', 'COARSE_INTERP', 'LIMIT_ENFORCER', 'SERVO_X', 'SERVO_Y', 'SERVO_Z', 'TELEM_RING'],
      Safety: ['SAFE_CH_A', 'SAFE_CH_B', 'SAFE_IO'],
    },
    cores: { MOTION: 1, SAFETY: 1 },
    dsl: `supervisor_graph! {
    executor MOTION;
    executor SAFETY;

    node PENDANT = Terminate, task: crate::hmi::pendant_task,
        writes: [signals::PROGRAM observed];

    node INTERPRETER = Terminate, deps: [PENDANT],
        task: crate::plan::interpreter_task,
        reads: [signals::PROGRAM], writes: [signals::PATH observed];

    node PATH_PLANNER = Terminate, deps: [INTERPRETER],
        task: crate::plan::planner_task,
        reads: [signals::PATH], writes: [signals::PLANNED observed];

    node IK_SOLVER = Terminate, deps: [PATH_PLANNER],
        task: crate::plan::ik_task,
        reads: [signals::PLANNED], writes: [signals::JOINTS observed];

    node SEGMENT_Q = Terminate, deps: [IK_SOLVER],
        task: crate::plan::segment_queue_task,
        reads: [signals::JOINTS], writes: [signals::SEGMENTS observed];

    node VISION = Terminate, task: crate::vision::camera_task,
        writes: [signals::FRAMES observed];

    node VISION_Q = Terminate, deps: [VISION],
        task: crate::vision::frame_pool_task,
        reads: [signals::FRAMES], writes: [signals::DETECTIONS observed];

    node LOGGER = Terminate, task: crate::log::logger_task,
        reads: [signals::TELEMETRY];

    node ECAT_MASTER = Terminate, executor: MOTION,
        task: crate::ecat::master_task,
        resources: [ECAT_IF: consume crate::ecat::EcatIf],
        provides: [PDO], slot_timeout: 3000;

    node COARSE_INTERP = Terminate, executor: MOTION,
        deps: [ECAT_MASTER ready, SEGMENT_Q],
        task: crate::motion::coarse_task,
        resources: [PDO: shared crate::ecat::Pdo], slot_timeout: 4000,
        reads: [signals::SEGMENTS], writes: [signals::SETPOINTS observed];

    node LIMIT_ENFORCER = Terminate, executor: MOTION,
        deps: [COARSE_INTERP, SAFE_IO ready bound],
        task: crate::motion::limits_task, slot_timeout: 4000,
        reads: [signals::SETPOINTS], writes: [signals::SAFE_SET observed];

    node SERVO_X = Terminate, executor: MOTION,
        deps: [LIMIT_ENFORCER ready bound],
        task: crate::motion::servo_task, slot_timeout: 4000,
        ready_on_write, beat_timeout: 600,
        reads: [signals::SAFE_SET], writes: [signals::ACTUAL observed beat];

    node SERVO_Y = Terminate, executor: MOTION,
        deps: [LIMIT_ENFORCER ready bound],
        task: crate::motion::servo_task, slot_timeout: 4000,
        ready_on_write, beat_timeout: 600,
        reads: [signals::SAFE_SET], writes: [signals::ACTUAL observed beat];

    node SERVO_Z = Terminate, executor: MOTION,
        deps: [LIMIT_ENFORCER ready bound],
        task: crate::motion::servo_task, slot_timeout: 4000,
        ready_on_write, beat_timeout: 600,
        reads: [signals::SAFE_SET], writes: [signals::ACTUAL observed beat];

    node TELEM_RING = Terminate, executor: MOTION, deps: [SERVO_X],
        task: crate::motion::telemetry_task, slot_timeout: 4000,
        reads: [signals::ACTUAL], writes: [signals::TELEMETRY observed];

    node SAFE_CH_A = Terminate, executor: SAFETY,
        task: crate::safety::channel_task,
        beat_timeout: 300, writes: [signals::CH_A observed beat];

    node SAFE_CH_B = Terminate, executor: SAFETY,
        task: crate::safety::channel_task,
        beat_timeout: 300, writes: [signals::CH_B observed beat];

    node SAFE_IO = Terminate, executor: SAFETY,
        deps: [SAFE_CH_A ready bound, SAFE_CH_B ready bound],
        task: crate::safety::sto_task, slot_timeout: 2000,
        reads: [signals::CH_A, signals::CH_B];
}`,
    behaviors: {
      nodes: {
        PENDANT: { kind: 'periodic', period_ms: 500, scaled: true },
        INTERPRETER: { kind: 'pipeline', work_ms: 300 },
        PATH_PLANNER: { kind: 'pipeline', work_ms: 350 },
        IK_SOLVER: { kind: 'pipeline', work_ms: 300 },
        SEGMENT_Q: { kind: 'queue', capacity: 8, policy: 'backpressure', drain_ms: 600 },
        VISION: { kind: 'periodic', period_ms: 150, scaled: true },
        VISION_Q: { kind: 'queue', capacity: 4, policy: 'reject', drain_ms: 120 },
        LOGGER: { kind: 'pipeline', work_ms: 400 },
        ECAT_MASTER: { kind: 'provider', startup_ms: 900 },
        COARSE_INTERP: { kind: 'control_loop', period_ms: 150 },
        LIMIT_ENFORCER: { kind: 'control_loop', period_ms: 150 },
        SERVO_X: { kind: 'control_loop', period_ms: 100 },
        SERVO_Y: { kind: 'control_loop', period_ms: 100 },
        SERVO_Z: { kind: 'control_loop', period_ms: 100 },
        TELEM_RING: { kind: 'queue', capacity: 10, policy: 'drop_oldest', drain_ms: 25 },
        SAFE_CH_A: { kind: 'periodic', period_ms: 150 },
        SAFE_CH_B: { kind: 'periodic', period_ms: 150 },
        SAFE_IO: { kind: 'control_loop', period_ms: 150 },
      },
      escalation: {
        SAFE_CH_A: 'clear_ready',
        SAFE_CH_B: 'clear_ready',
      },
    },
    devices: [
      {
        kind: 'slider',
        target: 'PENDANT',
        label: 'Program feed',
        initial: 0.7,
        hint: 'at rest the motion tier keeps up; slide to 1.0 and the segment queue fills until it back-pressures the planner; to 0 and it starves',
      },
      {
        kind: 'slider',
        target: 'VISION',
        label: 'Camera rate',
        min: 0,
        max: 2,
        initial: 1,
        unit: '×',
        hint: 'frames per camera tick; past about 1.25 the frame pool cannot keep up and rejects',
      },
      {
        kind: 'gauge',
        target: 'signals::SEGMENTS',
        label: 'Segment queue',
        source: 'depth',
        max: 8,
        hint: 'back-pressures the planner when full: dropping a segment means moving through unplanned geometry',
      },
      {
        kind: 'gauge',
        target: 'signals::DETECTIONS',
        label: 'Vision frame pool',
        source: 'depth',
        max: 4,
        hint: 'rejects when full: the camera clock cannot be told to wait, so surplus frames are refused',
      },
      {
        kind: 'gauge',
        target: 'signals::TELEMETRY',
        label: 'Telemetry ring',
        source: 'depth',
        max: 10,
        hint: 'drops oldest when full: blocking the RT tier is worse than losing a sample; stop the logger from its card and watch it fill',
      },
    ],
  },
  {
    id: 'av-camera',
    title: 'Edge A/V streaming head',
    group: 'systems',
    mechanisms:
      'Leased frame pool with two-phase drain, lend / consume / shared all load-bearing, OnDemand substream via start_node, per-client session pool, ready_on_write on encoder and timestamper, hold-last-good clock servo',
    blurb:
      'Capture and audio on their own tier; the media tier built around a refcounted frame pool (stopping the allocator refuses new frames, waits out the holders, then frees), a consume encoder channel that loses its state on rebuild, and per-client RTP sessions that drop their own frames rather than back-pressure the shared encoder. Kill the encoder and it is drained, rebuilt and re-keyed while sessions survive; lose PTP and the streams stay smooth while the clock servo holds last-good and drifts: the failure a heartbeat cannot catch.',
    planes: {
      Capture: ['VI_CAPTURE', 'AUDIO_CAP', 'PTP_TS'],
      Media: ['VB_ALLOC', 'AAA_LOOP', 'VPSS_MAIN', 'VENC_PRIMARY', 'VPSS_SUB', 'VENC_SECONDARY', 'AV_TIMESTAMPER', 'PTP_SERVO'],
      Serving: ['RTSP', 'SESSION_Q', 'RTP_SESSION', 'RECORDER', 'STORAGE_GC', 'ONVIF'],
    },
    cores: { CAPTURE: 1, MEDIA: 1 },
    dsl: `supervisor_graph! {
    executor CAPTURE;
    executor MEDIA;

    node VI_CAPTURE = Terminate, executor: CAPTURE,
        task: crate::vi::capture_task,
        resources: [VI_PIPE: consume crate::vi::ViPipe],
        beat_timeout: 400,
        writes: [signals::RAW_FRAMES observed beat];

    node AUDIO_CAP = Terminate, executor: CAPTURE,
        task: crate::audio::capture_task,
        writes: [signals::PCM observed];

    node PTP_TS = Terminate, executor: CAPTURE,
        task: crate::ptp::hw_stamp_task,
        writes: [signals::PHC observed];

    node VB_ALLOC = Terminate, executor: MEDIA,
        task: crate::vb::pool_task,
        writes: [signals::FRAMES];

    node AAA_LOOP = Terminate, executor: MEDIA, deps: [VI_CAPTURE],
        task: crate::isp::aaa_task,
        resources: [SENSOR_I2C: crate::isp::SensorI2c],
        reads: [signals::RAW_FRAMES],
        writes: [signals::EXPOSURE observed];

    node VPSS_MAIN = Terminate, executor: MEDIA,
        deps: [VI_CAPTURE, VB_ALLOC],
        task: crate::vpss::scale_task,
        reads: [signals::RAW_FRAMES, signals::FRAMES],
        writes: [signals::SCALED observed];

    node VENC_PRIMARY = Terminate, executor: MEDIA,
        deps: [AAA_LOOP ready, VPSS_MAIN], slot_timeout: 4000,
        task: crate::venc::encode_task,
        resources: [VENC_CH: consume crate::venc::Channel],
        ready_on_write, beat_timeout: 800,
        reads: [signals::SCALED],
        writes: [signals::BITSTREAM observed beat];

    node VPSS_SUB = OnDemand, executor: MEDIA,
        deps: [VI_CAPTURE, VB_ALLOC],
        task: crate::vpss::sub_scale_task,
        reads: [signals::RAW_FRAMES, signals::FRAMES],
        writes: [signals::SUB_SCALED observed];

    node VENC_SECONDARY = OnDemand, executor: MEDIA,
        deps: [VPSS_SUB ready], slot_timeout: 3000,
        task: crate::venc::sub_encode_task,
        reads: [signals::SUB_SCALED],
        writes: [signals::SUB_BITSTREAM observed];

    node PTP_SERVO = Terminate, deps: [PTP_TS],
        task: crate::ptp::servo_task,
        reads: [signals::PHC],
        writes: [signals::CLOCK observed];

    node AV_TIMESTAMPER = Terminate, executor: MEDIA,
        deps: [PTP_SERVO ready], slot_timeout: 3000,
        task: crate::av::timestamp_task,
        ready_on_write, beat_timeout: 800,
        reads: [signals::BITSTREAM, signals::PCM, signals::CLOCK],
        writes: [signals::AV observed beat];

    node RTSP = Terminate, deps: [AV_TIMESTAMPER],
        task: crate::rtsp::server_task, slot_timeout: 5000;

    node SESSION_Q = Terminate, deps: [AV_TIMESTAMPER],
        task: crate::rtp::session_queue_task,
        reads: [signals::AV],
        writes: [signals::TX observed];

    pool RTP_SESSION = [OnDemand, OnDemand, OnDemand, OnDemand],
        deps: [RTSP ready], slot_timeout: 3000,
        task: crate::rtp::sender_task,
        policy: DeferredShrink::new(Duration::from_secs(2)),
        min: 0, max: 4,
        reads: [signals::TX];

    node RECORDER = Pause, deps: [AV_TIMESTAMPER],
        task: crate::rec::segment_task,
        reads: [signals::AV, signals::SUB_BITSTREAM],
        writes: [signals::SEGMENTS observed];

    node STORAGE_GC = Terminate, deps: [RECORDER],
        task: crate::rec::gc_task,
        reads: [signals::SEGMENTS];

    node ONVIF = Terminate, task: crate::onvif::server_task;
}`,
    behaviors: {
      VI_CAPTURE: { kind: 'periodic', period_ms: 100 },
      AUDIO_CAP: { kind: 'periodic', period_ms: 250 },
      PTP_TS: { kind: 'periodic', period_ms: 500 },
      VB_ALLOC: { kind: 'periodic', period_ms: 200 },
      AAA_LOOP: { kind: 'control_loop', period_ms: 150 },
      VPSS_MAIN: { kind: 'lease_user', lease: 'signals::FRAMES', hold_ms: 120 },
      VENC_PRIMARY: { kind: 'pipeline', work_ms: 120 },
      VPSS_SUB: { kind: 'lease_user', lease: 'signals::FRAMES', hold_ms: 150 },
      VENC_SECONDARY: { kind: 'pipeline', work_ms: 150 },
      PTP_SERVO: { kind: 'control_loop', period_ms: 400 },
      AV_TIMESTAMPER: { kind: 'pipeline', work_ms: 100 },
      RTSP: { kind: 'link', initially_up: true },
      SESSION_Q: { kind: 'queue', capacity: 8, policy: 'drop_oldest', drain_ms: 100 },
      RTP_SESSION: { kind: 'session', busy_ms: 300 },
      RECORDER: { kind: 'pipeline', work_ms: 300 },
      STORAGE_GC: { kind: 'pipeline', work_ms: 800 },
      ONVIF: { kind: 'idle' },
    },
    devices: [
      {
        kind: 'dial',
        target: 'RTP_SESSION',
        label: 'Streaming clients',
        min: 0,
        max: 4,
        initial: 1,
        hint: 'one sender per client; each new join would force an IDR on a real head',
      },
      {
        kind: 'button',
        target: 'VPSS_SUB',
        label: 'Substream: start scaler',
        action: { type: 'node', node: 'VPSS_SUB', op: 'start' },
        hint: 'a 0-to-1 substream subscriber demand-starts the scaler',
      },
      {
        kind: 'button',
        target: 'VENC_SECONDARY',
        label: 'Substream: start encoder',
        action: { type: 'node', node: 'VENC_SECONDARY', op: 'start' },
        hint: 'gated on the scaler being ready',
      },
      {
        kind: 'button',
        target: 'VENC_CH',
        label: 'Rebuild encoder channel',
        action: { type: 'resource', resource: 'VENC_CH', cmd: 'provide' },
        hint: 'a killed encoder consumed its channel: re-create it, then restart the node',
      },
      {
        kind: 'gauge',
        target: 'signals::TX',
        label: 'Session queue',
        source: 'depth',
        max: 8,
        hint: 'drains as fast as the clients take: with none connected the backlog stands, and a slow client leaks its own frames rather than back-pressuring the shared encoder',
      },
    ],
  },
  {
    id: 'substation-ied',
    title: 'Substation protection IED',
    group: 'systems',
    mechanisms:
      'two planes with asymmetric failure, ready bound on PTP lock, ready_on_write sample alignment, OnDemand breaker-failure arming, Pause autoreclose, min:0 MMS sessions, a real VetoGate on the trip signal',
    blurb:
      'An IEC 61850 relay whose two networks fail differently. Differential protection needs matching time sync, so losing PTP bound-stops it while overcurrent keeps protecting. Drop the station bus instead and the protection plane never moves: SCADA goes blind, the relay keeps tripping. TRIP is a veto gate: each protection function asserts its own bit, any bit forces the safe state, none owns it, and a function that bound-stops with its bit up keeps the breaker open until it runs again: fail-safe by construction.',
    planes: {
      'Process bus': ['SV_RX_MU1', 'SV_RX_MU2', 'SV_RX_MU3', 'SV_ALIGN', 'PROT_5051', 'PROT_87', 'PROT_50BF', 'PROT_79', 'TRIP_LOGIC', 'GOOSE_PUB'],
      Station: ['STATION_LINK', 'MMS', 'DIST_REC', 'SUPERVISION'],
    },
    cores: { PROCESS_BUS: 1 },
    dsl: `supervisor_graph! {
    executor PROCESS_BUS;

    node PTP_SLAVE = Terminate, task: crate::ptp::slave_task;

    node SV_RX_MU1 = Terminate, executor: PROCESS_BUS,
        task: crate::sv::rx_task, writes: [signals::SV1 observed];
    node SV_RX_MU2 = Terminate, executor: PROCESS_BUS,
        task: crate::sv::rx_task, writes: [signals::SV2 observed];
    node SV_RX_MU3 = Terminate, executor: PROCESS_BUS,
        task: crate::sv::rx_task, writes: [signals::SV3 observed];

    node SV_ALIGN = Terminate, executor: PROCESS_BUS,
        deps: [SV_RX_MU1, SV_RX_MU2, SV_RX_MU3],
        task: crate::sv::align_task,
        ready_on_write, beat_timeout: 500,
        reads: [signals::SV1, signals::SV2, signals::SV3],
        writes: [signals::PHASORS observed beat];

    node PROT_5051 = Terminate, executor: PROCESS_BUS,
        deps: [SV_ALIGN ready], slot_timeout: 3000,
        task: crate::prot::overcurrent_task,
        reads: [signals::PHASORS], writes: [signals::TRIP veto observed];

    node PROT_87 = Terminate, executor: PROCESS_BUS,
        deps: [SV_ALIGN ready, PTP_SLAVE ready bound], slot_timeout: 3000,
        task: crate::prot::differential_task,
        reads: [signals::PHASORS], writes: [signals::TRIP veto observed];

    node PROT_50BF = OnDemand, executor: PROCESS_BUS,
        task: crate::prot::breaker_failure_task,
        reads: [signals::TRIP], writes: [signals::BF_TRIP observed];

    node PROT_79 = Pause, executor: PROCESS_BUS,
        task: crate::prot::autoreclose_task,
        reads: [signals::TRIP];

    node TRIP_LOGIC = Terminate, executor: PROCESS_BUS,
        deps: [PROT_5051],
        task: crate::prot::trip_matrix_task,
        reads: [signals::TRIP, signals::BF_TRIP],
        writes: [signals::GOOSE observed];

    node GOOSE_PUB = Terminate, executor: PROCESS_BUS,
        deps: [TRIP_LOGIC],
        task: crate::goose::publish_task,
        reads: [signals::GOOSE];

    node STATION_LINK = Terminate, task: crate::station::link_task,
        provides: [STATION_BUS], slot_timeout: 5000;

    pool MMS = [OnDemand, OnDemand, OnDemand],
        deps: [STATION_LINK ready bound], slot_timeout: 2000,
        task: crate::mms::session_task,
        policy: DeferredShrink::new(Duration::from_secs(2)),
        min: 0, max: 3,
        resources: [STATION_BUS: shared crate::station::Bus],
        reads: [signals::GOOSE, signals::COMTRADE];

    node DIST_REC = OnDemand,
        task: crate::rec::comtrade_task,
        reads: [signals::PHASORS], writes: [signals::COMTRADE observed];

    node SUPERVISION = Terminate, task: crate::sup::supervision_task,
        reads: [signals::SV1];
}`,
    behaviors: {
      PTP_SLAVE: { kind: 'link', initially_up: true },
      'crate::sv::rx_task': { kind: 'periodic', period_ms: 100 },
      SV_ALIGN: { kind: 'pipeline', work_ms: 100 },
      PROT_5051: { kind: 'veto_writer', period_ms: 150 },
      PROT_87: { kind: 'veto_writer', period_ms: 150 },
      PROT_50BF: { kind: 'control_loop', period_ms: 200 },
      PROT_79: { kind: 'control_loop', period_ms: 400 },
      TRIP_LOGIC: { kind: 'veto_sink', period_ms: 150 },
      GOOSE_PUB: { kind: 'pipeline', work_ms: 100 },
      STATION_LINK: { kind: 'link', initially_up: true },
      MMS: { kind: 'session', busy_ms: 400 },
      DIST_REC: { kind: 'pipeline', work_ms: 200 },
      SUPERVISION: { kind: 'control_loop', period_ms: 300 },
    },
    devices: [
      {
        kind: 'switch',
        target: 'PROT_5051',
        label: 'Overcurrent pickup',
        onLabel: 'tripping',
        offLabel: 'reset',
        initial: 0,
        hint: "asserts this function's bit of the TRIP veto gate; the trip matrix publishes GOOSE while any bit is up",
      },
      {
        kind: 'switch',
        target: 'PROT_87',
        label: 'Differential pickup',
        onLabel: 'tripping',
        offLabel: 'reset',
        initial: 0,
        hint: 'trip, then lose PTP: the bound-stopped function cannot release its bit, so the trip holds',
      },
      {
        kind: 'switch',
        target: 'PTP_SLAVE',
        label: 'PTP lock',
        onLabel: 'locked',
        offLabel: 'lost',
        initial: 1,
        hint: 'differential needs matching sync across merging units; overcurrent does not',
      },
      {
        kind: 'switch',
        target: 'STATION_LINK',
        label: 'Station bus',
        initial: 1,
        hint: 'SCADA visibility only: the protection plane never moves',
      },
      {
        kind: 'dial',
        target: 'MMS',
        label: 'SCADA clients',
        min: 0,
        max: 3,
        initial: 0,
        hint: 'client sessions with a real cap',
      },
      {
        kind: 'button',
        target: 'PROT_50BF',
        label: 'Arm breaker failure',
        action: { type: 'node', node: 'PROT_50BF', op: 'start' },
        hint: 'armed by the first trip on a real relay: an OnDemand start',
      },
      {
        kind: 'button',
        target: 'DIST_REC',
        label: 'Trigger disturbance record',
        action: { type: 'node', node: 'DIST_REC', op: 'start' },
        hint: 'a fault record capture, demand-started',
      },
    ],
  },
  {
    id: 'patient-monitor',
    title: 'Multi-parameter patient monitor',
    group: 'systems',
    mechanisms:
      'runtime module insert/remove via start_node/stop_node, alarm arbiter as a hard ready gate, Leased audio bursts, consume NIBP pneumatics with a privileged safety monitor, ready_on_write patient context, provider learning phase',
    blurb:
      'The only system that changes shape at runtime: parameter modules are OnDemand subtrees inserted and removed from the buttons, and their absence is announced, not silent. The patient context must publish before any detector runs: adult versus neonate changes every alarm limit. Every detector gates on the alarm arbiter, the audio codec is leased per burst, and the NIBP pneumatics are consumed by each measurement cycle while an always-on safety monitor watches.',
    planes: {
      Context: ['PATIENT_CONTEXT', 'ALARM_ARBITER', 'AUDIO_CODEC'],
      ECG: ['ECG_ACQ', 'ECG_DSP', 'ECG_ALARM', 'ARRHYTHMIA'],
      SpO2: ['SPO2_ACQ', 'SPO2_DSP', 'SPO2_ALARM'],
      NIBP: ['NIBP_SM', 'NIBP_SAFETY'],
      Output: ['TRENDS', 'DISPLAY'],
    },
    dsl: `supervisor_graph! {
    node PATIENT_CONTEXT = Terminate,
        task: crate::adm::context_task,
        ready_on_write, beat_timeout: 1000,
        writes: [signals::CONTEXT observed beat];

    node AUDIO_CODEC = Terminate, task: crate::audio::codec_task,
        writes: [signals::AUDIO];

    node ALARM_ARBITER = Terminate,
        deps: [PATIENT_CONTEXT ready], slot_timeout: 4000,
        task: crate::alarm::arbiter_task,
        reads: [signals::ALARM_EVTS, signals::AUDIO],
        writes: [signals::ANNUNCIATE observed];

    node ECG_ACQ = Terminate,
        deps: [PATIENT_CONTEXT ready], slot_timeout: 4000,
        task: crate::ecg::acquire_task,
        writes: [signals::ECG observed];

    node ECG_DSP = Terminate, deps: [ECG_ACQ],
        task: crate::ecg::dsp_task,
        reads: [signals::ECG], writes: [signals::HR observed];

    node ECG_ALARM = Terminate,
        deps: [ECG_DSP, ALARM_ARBITER ready], slot_timeout: 4000,
        task: crate::ecg::alarm_task,
        reads: [signals::HR], writes: [signals::ALARM_EVTS observed];

    node ARRHYTHMIA = Terminate, deps: [ECG_DSP],
        task: crate::ecg::arrhythmia_task,
        reads: [signals::HR], writes: [signals::RHYTHM observed];

    node SPO2_ACQ = OnDemand, task: crate::spo2::acquire_task,
        writes: [signals::PLETH observed];

    node SPO2_DSP = OnDemand, deps: [SPO2_ACQ ready bound],
        task: crate::spo2::dsp_task, slot_timeout: 2000,
        reads: [signals::PLETH], writes: [signals::SPO2 observed];

    node SPO2_ALARM = OnDemand,
        deps: [SPO2_DSP ready bound, ALARM_ARBITER ready], slot_timeout: 2000,
        task: crate::spo2::alarm_task,
        reads: [signals::SPO2], writes: [signals::ALARM_EVTS observed];

    node NIBP_SM = OnDemand,
        task: crate::nibp::measure_task,
        resources: [NIBP_PNEUMATICS: consume crate::nibp::Pneumatics],
        writes: [signals::NIBP observed];

    node NIBP_SAFETY = Terminate, task: crate::nibp::safety_task,
        reads: [signals::NIBP];

    node TRENDS = Terminate, task: crate::ui::trends_task,
        reads: [signals::HR, signals::SPO2, signals::NIBP];

    node DISPLAY = Terminate, task: crate::ui::display_task,
        reads: [signals::HR];
}`,
    behaviors: {
      PATIENT_CONTEXT: { kind: 'periodic', period_ms: 800 },
      AUDIO_CODEC: { kind: 'periodic', period_ms: 500 },
      ALARM_ARBITER: { kind: 'lease_user', lease: 'signals::AUDIO', hold_ms: 600 },
      ECG_ACQ: { kind: 'periodic', period_ms: 100 },
      ECG_DSP: { kind: 'pipeline', work_ms: 150 },
      ECG_ALARM: { kind: 'control_loop', period_ms: 300 },
      ARRHYTHMIA: { kind: 'provider', startup_ms: 2000 },
      SPO2_ACQ: { kind: 'periodic', period_ms: 150 },
      SPO2_DSP: { kind: 'pipeline', work_ms: 200 },
      SPO2_ALARM: { kind: 'control_loop', period_ms: 300 },
      NIBP_SM: { kind: 'oneshot', run_ms: 1500 },
      NIBP_SAFETY: { kind: 'control_loop', period_ms: 400 },
      TRENDS: { kind: 'pipeline', work_ms: 500 },
      DISPLAY: { kind: 'pipeline', work_ms: 300 },
    },
    devices: [
      {
        kind: 'slider',
        target: 'ECG_ACQ',
        label: 'Heart rate source',
        initial: 0.6,
        hint: 'the simulated patient',
      },
      {
        kind: 'button',
        target: 'SPO2_ACQ',
        label: 'SpO2: insert sensor',
        action: { type: 'node', node: 'SPO2_ACQ', op: 'start' },
        hint: 'plugging a module in demand-starts its subtree, stage by stage',
      },
      {
        kind: 'button',
        target: 'SPO2_DSP',
        label: 'SpO2: start processing',
        action: { type: 'node', node: 'SPO2_DSP', op: 'start' },
      },
      {
        kind: 'button',
        target: 'SPO2_ALARM',
        label: 'SpO2: register alarms',
        action: { type: 'node', node: 'SPO2_ALARM', op: 'start' },
        hint: 'gated on the arbiter: a detector firing into a void is a silent-alarm hazard',
      },
      {
        kind: 'button',
        target: 'SPO2_ACQ',
        label: 'SpO2: remove module',
        action: { type: 'node', node: 'SPO2_ACQ', op: 'stop' },
        hint: 'stopping the acquisition bound-stops the DSP and alarm stages: the absence is announced, not silent',
      },
      {
        kind: 'button',
        target: 'NIBP_SM',
        label: 'Start NIBP cycle',
        action: { type: 'node', node: 'NIBP_SM', op: 'start' },
        hint: 'the cycle consumes the pneumatics and runs to completion',
      },
      {
        kind: 'button',
        target: 'NIBP_PNEUMATICS',
        label: 'Re-arm cuff',
        action: { type: 'resource', resource: 'NIBP_PNEUMATICS', cmd: 'provide' },
        hint: 'a new cycle fails closed until the pneumatics come back',
      },
      {
        kind: 'lease',
        target: 'signals::AUDIO',
        label: 'Audio codec',
        hint: 'the arbiter holds it per burst: a truncated IEC 60601-1-8 melody is not a conformant signal',
      },
    ],
  },
  {
    id: 'cubesat-cfs',
    title: 'CubeSat flight software',
    group: 'systems',
    mechanisms:
      'scheduler gated on the major-frame tone, ready_on_write attitude estimate, Backed demand-start of a stored-command sequence, OnDemand apps started by ground command, reject software-bus pipe, watchdog + restart escalation',
    blurb:
      'The cFS shape: a scheduler gated on the 1 Hz major-frame tone, core services, and mission apps around them. The attitude chain will not act on a garbage quaternion: the estimator is ready_on_write and the controller gates on it. File manager, CFDP and the payload are started by ground command; the software-bus pipe rejects on overflow and never blocks the router; and a stalled app gets the first rung of the health ladder: a restart.',
    planes: {
      'Core services': ['TIME', 'SCH', 'SB_PIPE', 'EVS', 'TBL', 'HS', 'HK', 'CI', 'TO'],
      ADCS: ['ADCS_SENSE', 'ADCS_EST', 'ADCS_CTRL', 'ADCS_ACT'],
      Platform: ['EPS', 'THRM'],
      Mission: ['PAYLOAD', 'COMM', 'DS', 'CF', 'FM', 'LC', 'SC'],
    },
    dsl: `supervisor_graph! {
    node TIME = Terminate, task: crate::cfe::time_task,
        writes: [signals::TONE observed];

    node SCH = Terminate, deps: [TIME ready], slot_timeout: 6000,
        task: crate::cfe::sch_task,
        reads: [signals::TONE], writes: [signals::WAKEUP observed];

    node EVS = Terminate, task: crate::cfe::evs_task;
    node TBL = Terminate, task: crate::cfe::tbl_task;

    node HS = Terminate, task: crate::cfe::hs_task;

    node ADCS_SENSE = Terminate, deps: [SCH],
        task: crate::adcs::sense_task, beat_timeout: 800,
        reads: [signals::WAKEUP],
        writes: [signals::GYRO observed beat];

    node ADCS_EST = Terminate, deps: [ADCS_SENSE],
        task: crate::adcs::estimate_task,
        ready_on_write, beat_timeout: 1000,
        reads: [signals::GYRO],
        writes: [signals::ATT observed beat];

    node ADCS_CTRL = Terminate, deps: [ADCS_EST ready], slot_timeout: 4000,
        task: crate::adcs::control_task,
        reads: [signals::ATT], writes: [signals::TORQUE observed];

    node ADCS_ACT = Terminate, deps: [ADCS_CTRL],
        task: crate::adcs::actuate_task,
        reads: [signals::TORQUE];

    node EPS = Terminate, task: crate::plat::eps_task,
        writes: [signals::EPS_TLM observed];

    node THRM = Terminate, deps: [EPS],
        task: crate::plat::thermal_task,
        reads: [signals::EPS_TLM];

    node HK = Terminate, deps: [ADCS_EST, EPS],
        task: crate::cfe::hk_task,
        reads: [signals::ATT, signals::EPS_TLM],
        writes: [signals::HK_TLM observed];

    node SB_PIPE = Terminate, deps: [HK],
        task: crate::cfe::sb_task,
        reads: [signals::HK_TLM],
        writes: [signals::DOWN observed];

    node CI = Terminate, task: crate::cfe::ci_task,
        writes: [signals::CMDS observed];

    node TO = Terminate, deps: [COMM ready bound], slot_timeout: 1000,
        task: crate::cfe::to_task,
        reads: [signals::DOWN];

    node COMM = OnDemand, task: crate::com::radio_task;

    node PAYLOAD = OnDemand, task: crate::sci::payload_task,
        writes: [signals::SCIENCE observed];

    node DS = Terminate, task: crate::cfs::ds_task,
        reads: [signals::DOWN];

    node CF = OnDemand, task: crate::cfs::cf_task,
        reads: [signals::SCIENCE];

    node FM = OnDemand, task: crate::cfs::fm_task;

    node LC = Terminate, deps: [SCH],
        task: crate::cfs::lc_task,
        reads: [signals::SEQ];

    node SC = Terminate, disabled,
        task: crate::cfs::sc_task,
        writes: [signals::SEQ observed];
}`,
    behaviors: {
      nodes: {
        TIME: { kind: 'periodic', period_ms: 1000 },
        SCH: { kind: 'pipeline', work_ms: 100 },
        EVS: { kind: 'idle' },
        TBL: { kind: 'idle' },
        HS: { kind: 'watchdog', feed_ms: 500 },
        ADCS_SENSE: { kind: 'pipeline', work_ms: 200 },
        ADCS_EST: { kind: 'pipeline', work_ms: 250 },
        ADCS_CTRL: { kind: 'control_loop', period_ms: 200 },
        ADCS_ACT: { kind: 'pipeline', work_ms: 200 },
        EPS: { kind: 'periodic', period_ms: 600 },
        THRM: { kind: 'control_loop', period_ms: 800 },
        HK: { kind: 'pipeline', work_ms: 400 },
        SB_PIPE: { kind: 'queue', capacity: 6, policy: 'reject', drain_ms: 300 },
        CI: { kind: 'periodic', period_ms: 2000 },
        TO: { kind: 'pipeline', work_ms: 300 },
        COMM: { kind: 'link', initially_up: true },
        PAYLOAD: { kind: 'periodic', period_ms: 500 },
        DS: { kind: 'pipeline', work_ms: 500 },
        CF: { kind: 'pipeline', work_ms: 400 },
        FM: { kind: 'oneshot', run_ms: 1200 },
        LC: { kind: 'gated_consumer', open: 'signals::SEQ', period_ms: 400 },
        SC: { kind: 'periodic', period_ms: 600 },
      },
      escalation: {
        ADCS_SENSE: 'restart',
      },
    },
    devices: [
      {
        kind: 'button',
        target: 'COMM',
        label: 'AOS: start comm window',
        action: { type: 'node', node: 'COMM', op: 'start' },
        hint: 'the radio comes up for a pass; TO is bound to it',
      },
      {
        kind: 'button',
        target: 'COMM',
        label: 'LOS: end comm window',
        action: { type: 'node', node: 'COMM', op: 'stop' },
      },
      {
        kind: 'button',
        target: 'FM',
        label: 'Ground cmd: file manager',
        action: { type: 'node', node: 'FM', op: 'start' },
        hint: 'an OnDemand app started by command, the cFS way',
      },
      {
        kind: 'button',
        target: 'PAYLOAD',
        label: 'Ground cmd: payload on',
        action: { type: 'node', node: 'PAYLOAD', op: 'start' },
      },
      {
        kind: 'button',
        target: 'CF',
        label: 'Ground cmd: CFDP downlink',
        action: { type: 'node', node: 'CF', op: 'start' },
      },
      {
        kind: 'gauge',
        target: 'signals::DOWN',
        label: 'SB pipe',
        source: 'depth',
        max: 6,
        hint: 'rejects on overflow: the router is never blocked by a slow subscriber',
      },
    ],
  },
  {
    id: 'ev-site',
    title: 'EV charging site controller',
    group: 'systems',
    mechanisms:
      'divisible site budget (a real Budget: FairShare under ShrinkFastGrowSlow), session pool claiming shares, supervisor-side release of a stopped session, derate by re-providing, backpressure store-and-forward behind the CSMS link, privileged monitors bypassing the allocator',
    blurb:
      'One site power limit, continuously re-divided across active charging sessions. Plug a car in and every grant shrinks instantly; unplug and they grow back a step at a time. Stop a session from the outside and the supervisor releases its share on the shutdown ack: a dead session never strands its amps. The OCPP client queues transaction events in causal order while the CSMS link is down, and the RCD and thermal monitors bypass the allocator: a fault is not negotiable.',
    planes: {
      Site: ['GRID_METER', 'ENERGY_MGR', 'THERMAL', 'RCD'],
      Connectors: ['CP_STATE', 'EVSE'],
      Backoffice: ['OCPP', 'OCPP_TX', 'SAF_Q', 'LOCAL_AUTH'],
    },
    dsl: `supervisor_graph! {
    node GRID_METER = Terminate, task: crate::site::meter_task,
        writes: [signals::SITE_LOAD observed];

    node ENERGY_MGR = Terminate, deps: [GRID_METER],
        task: crate::site::energy_task,
        provides: [SITE_AMPS],
        reads: [signals::SITE_LOAD, signals::DERATE];

    node THERMAL = Terminate, task: crate::site::thermal_task,
        reads: [signals::SITE_LOAD],
        writes: [signals::DERATE observed];

    node RCD = Terminate, task: crate::safety::rcd_task,
        writes: [signals::TRIP_EVT observed];

    node CP_STATE = Terminate, task: crate::evse::pilot_task,
        writes: [signals::PILOT observed];

    pool EVSE = [Terminate, OnDemand, OnDemand, OnDemand],
        deps: [ENERGY_MGR], slot_timeout: 3000,
        task: crate::evse::session_task,
        policy: DeferredShrink::new(Duration::from_secs(2)),
        min: 1, max: 4,
        resources: [SITE_AMPS: divisible],
        reads: [signals::PILOT],
        writes: [signals::SESSION_EVTS observed];

    node SAF_Q = Terminate, deps: [EVSE],
        task: crate::ocpp::saf_task,
        reads: [signals::SESSION_EVTS],
        writes: [signals::OCPP_OUT observed];

    node OCPP = Terminate, task: crate::ocpp::client_task,
        provides: [CSMS], slot_timeout: 5000;

    node OCPP_TX = Terminate, deps: [OCPP ready bound], slot_timeout: 3000,
        task: crate::ocpp::tx_task,
        resources: [CSMS: shared crate::ocpp::Csms],
        reads: [signals::OCPP_OUT];

    node LOCAL_AUTH = Terminate, task: crate::auth::local_task,
        reads: [signals::PILOT];
}`,
    behaviors: {
      GRID_METER: { kind: 'periodic', period_ms: 400 },
      ENERGY_MGR: { kind: 'budget', total: 32, period_ms: 300, step: 4 },
      THERMAL: { kind: 'control_loop', period_ms: 800 },
      RCD: { kind: 'periodic', period_ms: 4000 },
      CP_STATE: { kind: 'periodic', period_ms: 500 },
      EVSE: { kind: 'session', busy_ms: 400 },
      SAF_Q: { kind: 'queue', capacity: 10, policy: 'backpressure', drain_ms: 120 },
      OCPP: { kind: 'link', initially_up: true },
      OCPP_TX: { kind: 'pipeline', work_ms: 300 },
      LOCAL_AUTH: { kind: 'control_loop', period_ms: 600 },
    },
    devices: [
      {
        kind: 'dial',
        target: 'EVSE',
        label: 'Cars plugged in',
        min: 0,
        max: 4,
        initial: 1,
        hint: 'each connected car claims a share of SITE_AMPS; the allocator re-divides on every claim',
      },
      {
        kind: 'gauge',
        target: 'EVSE#0',
        source: 'grant',
        label: 'First session grant',
        max: 32,
        unit: ' A',
        hint: 'cut the instant a claimant joins; grows back 4 A per period, never in one jump',
      },
      {
        kind: 'gauge',
        target: 'SITE_AMPS',
        source: 'granted',
        label: 'Site limit in use',
        max: 32,
        unit: ' A',
        hint: 'the sum of every grant, never above the provided capacity',
      },
      {
        kind: 'slider',
        target: 'ENERGY_MGR',
        label: 'Site limit',
        initial: 1,
        hint: 'a derate re-provides the budget with less: every grant above its new share is cut at once',
      },
      {
        kind: 'button',
        target: 'EVSE#1',
        label: 'Stop session 2',
        action: { type: 'node', node: 'EVSE#1', op: 'stop' },
        hint: 'a single-node stop: the supervisor releases its share on the shutdown ack, the worker never touches it',
      },
      {
        kind: 'switch',
        target: 'OCPP',
        label: 'CSMS link',
        initial: 1,
        hint: 'offline: transaction events queue in causal order and drain on reconnect',
      },
      {
        kind: 'gauge',
        target: 'signals::OCPP_OUT',
        label: 'Store-and-forward',
        source: 'depth',
        max: 10,
        hint: 'back-pressured: a transaction event must not be lost, so the producer waits',
      },
      {
        kind: 'slider',
        target: 'GRID_METER',
        label: 'Grid load',
        initial: 0.4,
        hint: 'what the site meter reads upstream of the chargers',
      },
    ],
  },
  {
    id: 'pool',
    group: 'mechanisms',
    mechanisms: 'elastic pools, mark_busy scale-out, DeferredShrink',
    title: 'Elastic pool under load',
    blurb:
      'One elastic worker pool and a load dial. Busy members request scale-out; the DeferredShrink policy waits out its cooldown before folding idle members back in.',
    dsl: `supervisor_graph! {
    node INTAKE = Terminate, task: crate::intake::intake_task,
        writes: [JOBS observed];

    pool CREW = [Terminate, OnDemand, OnDemand, OnDemand],
        task: crate::crew::worker_task,
        policy: DeferredShrink::new(Duration::from_secs(2)),
        min: 1, max: 4,
        reads: [JOBS];
}`,
    behaviors: {
      INTAKE: { kind: 'periodic', period_ms: 1000 },
      CREW: { kind: 'server', busy_ms: 400 },
    },
    devices: [
      { kind: 'dial', target: 'CREW', label: 'Job load', hint: '0 = idle, 1 = a job every tick', initial: 0.2 },
    ],
  },
  {
    id: 'recovery',
    group: 'mechanisms',
    mechanisms: 'liveness monitor, restart cascade, ShutdownTimeout',
    title: 'Failure and recovery',
    blurb:
      'A heartbeat chain watched by the liveness monitor. Stall the worker and the monitor reports it stale; wedge it and a restart runs into the shutdown-ack timeout, surfacing a real fault. Restart is a cascade: dependents stop first, in reverse order.',
    dsl: `supervisor_graph! {
    node SOURCE = Terminate, task: crate::source::source_task,
        beat_timeout: 800,
        writes: [FEED observed beat];

    node WORKER = Terminate, deps: [SOURCE],
        task: crate::worker::crunch_task,
        beat_timeout: 800,
        reads: [FEED], writes: [OUT observed beat];

    node SINK = Terminate, deps: [WORKER],
        task: crate::sink::sink_task,
        reads: [OUT];
}`,
    behaviors: {
      SOURCE: { kind: 'periodic', period_ms: 150 },
      WORKER: { kind: 'pipeline', work_ms: 200 },
      SINK: { kind: 'pipeline', work_ms: 300 },
    },
    devices: [],
  },
  {
    id: 'bringup',
    group: 'mechanisms',
    mechanisms: 'ready deps, provides, ready bound teardown/resume',
    title: 'Gated bring-up',
    blurb:
      "A slow provider and everything that waits on it: a ready dep holds consumers at the gate, a provided resource slot fills mid-flight, and a 'ready bound' link tears its dependents down when it drops and resumes them when it returns. Flip the link switch and watch the bound half follow.",
    dsl: `supervisor_graph! {
    node RADIO = Terminate, task: crate::radio::radio_task,
        provides: [RADIO_DEV], slot_timeout: 8000;

    node LINK = Terminate, deps: [RADIO ready],
        task: crate::link::link_task, slot_timeout: 8000,
        beat_timeout: 1500;

    node TELEMETRY = Terminate, deps: [LINK ready bound],
        task: crate::telemetry::telemetry_task,
        resources: [RADIO_DEV: crate::radio::Radio],
        slot_timeout: 8000,
        writes: [TLM observed];

    node COMMANDS = Terminate, deps: [LINK ready bound],
        task: crate::commands::command_task,
        slot_timeout: 8000,
        reads: [TLM];
}`,
    behaviors: {
      RADIO: { kind: 'provider', startup_ms: 1200 },
      LINK: { kind: 'link', initially_up: true },
      TELEMETRY: { kind: 'periodic', period_ms: 200 },
      COMMANDS: { kind: 'pipeline', work_ms: 350 },
    },
    devices: [
      { kind: 'switch', target: 'LINK', label: 'Radio link', hint: 'bound deps stop when it drops, resume when it returns', initial: 1 },
    ],
  },
  {
    id: 'demand',
    group: 'mechanisms',
    mechanisms: 'Backed gates, demand-start through the control queue, counted Open guards, producer retirement',
    title: 'Demand-driven bring-up',
    blurb:
      'No deps: anywhere. Every producer sleeps disabled until the first gated read opens its signal; the open demand-starts it through the real control queue, cascading up the data chain in staggered waves you can watch arrive. Ordering emerges from data, not declarations. Each open is a counted guard: stop the dashboard from its card and, three seconds after its last reader leaves, the twin store retires itself through the same queue, which lets go of the modem, which retires in turn: the waves run back down.',
    dsl: `supervisor_graph! {
    node MODEM = Terminate, task: crate::modem::modem_task,
        disabled, writes: [LINK_STATE observed];

    node SENSOR_HUB = Terminate, task: crate::hub::hub_task,
        disabled, writes: [TELEMETRY observed];

    node TWIN_STORE = Terminate, task: crate::twin::twin_task,
        disabled, reads: [LINK_STATE], writes: [DEVICE_TWIN observed];

    node CLOUD_SYNC = Terminate, task: crate::cloud::sync_task,
        reads: [TELEMETRY];

    node DASHBOARD = Terminate, task: crate::dash::dash_task,
        reads: [DEVICE_TWIN];
}`,
    behaviors: {
      MODEM: { kind: 'periodic', period_ms: 400, retire_ms: 3000 },
      SENSOR_HUB: { kind: 'periodic', period_ms: 150, retire_ms: 3000 },
      TWIN_STORE: { kind: 'gated_consumer', open: 'LINK_STATE', period_ms: 300, retire_ms: 3000 },
      CLOUD_SYNC: { kind: 'gated_consumer', open: 'TELEMETRY', period_ms: 300, delay_ms: 2500 },
      DASHBOARD: { kind: 'gated_consumer', open: 'DEVICE_TWIN', period_ms: 500, delay_ms: 5000 },
    },
    devices: [],
  },
  {
    id: 'leases',
    group: 'mechanisms',
    mechanisms: 'Leased signals, drain and reopen',
    title: 'Config leases and rollover',
    blurb:
      'A Leased configuration signal: workers hold live leases (watch the gauge), and a rollover drains them all, swaps the value, then reopens the tap. Nothing stops; the hand-off is coordinated by the lease count alone.',
    dsl: `supervisor_graph! {
    node CONFIG_STORE = Terminate,
        task: crate::config::store_task,
        writes: [CONFIG];

    node PID_LOOP = Terminate, deps: [CONFIG_STORE],
        task: crate::pid::pid_task, reads: [CONFIG];

    node REPORTER = Terminate, deps: [CONFIG_STORE],
        task: crate::report::report_task, reads: [CONFIG];

    node UPLINK = Terminate, deps: [CONFIG_STORE],
        task: crate::uplink::uplink_task, reads: [CONFIG];
}`,
    behaviors: {
      CONFIG_STORE: { kind: 'periodic', period_ms: 800 },
      PID_LOOP: { kind: 'lease_user', lease: 'CONFIG', hold_ms: 600 },
      REPORTER: { kind: 'lease_user', lease: 'CONFIG', hold_ms: 900 },
      UPLINK: { kind: 'lease_user', lease: 'CONFIG', hold_ms: 400 },
    },
    devices: [
      { kind: 'lease', target: 'CONFIG', label: 'CONFIG rollover', hint: 'drain all leases, then reopen' },
    ],
  },
];
