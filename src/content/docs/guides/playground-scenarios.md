---
title: 'The playground scenarios'
description: 'What each playground scenario models and which supervisor mechanisms it exercises.'
---

The scenario picker splits in two. **Mechanisms** are guided tours of one
crate feature each: pools, recovery, gated bring-up, demand-start, leases.
**Systems** are architectures of real embedded products at close to real
scale, grounded in public documentation and shipped source, cited below.

Every system carries a differently shaped contended resource (a serialized
bus, a bounded spool, a divisible power budget, a severity-arbitrated
annunciator), because no single mechanism covers them all.

## Cellular asset tracker

The archetype where **energy, not time, is the scarce resource**: a
battery-powered tracker that sleeps most of its life, wakes on motion or a
schedule, and uploads.

The sleep/wake cycle runs the README's power-coordinator recipe. `POWER` has
no `task:`. The application spawns it, because only the app holds the
`Spawner` that `respawn_terminate` takes; it detaches itself so its own
`teardown()` skips it. **Sleep** is a reverse-order `teardown()`: the `Pause`
sensors park keeping their state, and the modem's `consume` runner is dropped
for good. **Wake** is `resume_pausable()` (parked tasks resume in place) plus
`respawn_terminate()` (stateless services respawn in dependency order).

Wake without rebuilding the modem runner fails closed on the empty `consume`
slot; `Rebuild modem runner` re-provides it. GNSS carries `ready_on_write`:
it is ready while a fix is actually being produced, not merely because the
task started. The `disabled` FOTA latch and the detached run-once self-test
both survive the cycle.

Sources:
[Telit on tracker design choices](https://www.telit.com/blog/global-iot-asset-tracking-devices-key-design-choices/) ·
[1NCE on PSM and eDRX](https://www.1nce.com/en-us/resources/news/blog/psm-and-edrx) ·
[DigiKey on NB-IoT / Cat-M power saving](https://www.digikey.com/en/articles/how-to-enable-power-saving-modes-of-nb-iot-and-cat-m) ·
[LwM2M firmware update object](https://github.com/OpenMobileAlliance/lwm2m-registry/blob/prod/5.xml) ·
[Memfault on OTA for IoT](https://memfault.com/blog/ota-for-iot/) ·
[GNSS/cellular coexistence blanking](https://patents.google.com/patent/US20180035444A1/en)

## Industrial edge gateway

A protocol-translating gateway in the four-plane split real products use:
field drivers, a tag database, a filter stage, and a cloud session northbound.

An RS-485 bus owner (`Pause`), an **elastic pool of Modbus pollers** (one per
device group, all contending for the one half-duplex bus `BUS485: shared`),
a tag database gated on the pool's floor member, a deadband filter, a bounded
store-and-forward spool (`drop_oldest`: the uplink is opportunistic, the field
bus is clock-driven), and a **singleton** Sparkplug session
(`deps: [MODEM ready bound]`).

The pooling is deliberately southbound: a real MQTT client is one ordered
session, and a pool of those is a shape nobody should copy. The elasticity
real gateways have lives in driver instances.

Try: drop the uplink. The session bound-stops, polling continues, the spool
climbs and caps at capacity; restore it and the backlog drains. Turn the
device dial up and the poll pool grows; turn it down and `DeferredShrink`
folds it back.

Sources:
[MIGS modular edge gateway](https://www.ncbi.nlm.nih.gov/pmc/articles/PMC12788308/) ·
[TI Sitara IIoT gateway (Modbus acquisition cycle)](https://www.ncbi.nlm.nih.gov/pmc/articles/PMC11014140/) ·
[Modbus polling optimization](https://machinecdn.com/blog/2026/03/01/modbus-polling-optimization-register-grouping-guide/) ·
[Diagnosing Modbus degradation](https://flowfuse.com/blog/2026/04/diagnosing-modbus-degradation/) ·
[RS-485 multi-slave performance](https://industrialmonitordirect.com/blogs/knowledgebase/modbus-rtu-rs-485-multiple-slave-performance-optimization) ·
[Modbus exception responses](https://store.chipkin.com/articles/modbus-exception-responses) ·
[Sparkplug topic namespace and state management](https://www.eclipse.org/tahu/spec/Sparkplug%20Topic%20Namespace%20and%20State%20ManagementV2.2-with%20appendix%20B%20format%20-%20Eclipse.pdf) ·
[HiveMQ on Sparkplug session management](https://www.hivemq.com/blog/mqtt-sparkplug-session-management-iiot/) ·
[OPC UA MonitoredItem queue overflow](https://reference.opcfoundation.org/specs/OPC-10000-4/5.13.1) ·
[reconnect backoff with jitter](https://www.hivemq.com/blog/hivemq-mqtt-client-features-reconnect-handling/) ·
[MQTT persistent sessions](https://www.emqx.com/en/blog/mqtt-session)

## Battery management system

The safety ladder certified BMS firmware runs: warn, limit charge, limit
discharge, derate, open the contactors. Protection sits on its own executor
tier, the way products separate a safety base layer from the application
layer.

The safety tier carries the samplers and the protection loop under tight
liveness budgets. `PRECHARGE` provides `HV_BUS` only once the DC link reaches
threshold inside its `slot_timeout:`. Closing a contactor early welds it,
and a timeout is a latched fault. `CONTACTORS` sits behind
`deps: [PRECHARGE ready, PROTECT ready bound]`.

The escalation policy is the point. Stall the protection loop and the
application withdraws its readiness (`clear_ready`), which opens the
contactors through the `ready bound` edge (the safe state) instead of
restarting a stalled loop with the pack live. Stall the SoC estimator instead
and the policy activates the `disabled` LIMP limiter, a second writer of the
same limits signal.

Cell balancing is a `min: 0` pool under a thermal budget: at most four
channels bleed at once, one stays warm at rest, and the pool follows the
imbalance dial.

Sources:
[Battery Design on BMS architecture](https://www.batterydesign.net/battery-management-system/) ·
[Lithium Balance n3-BMS (segregated safety layer)](https://lithiumbalance.com/products/n3-bms-battery-management-system-bms/) ·
[TI TIDA-020076 HV BMS reference design](https://www.ti.com/lit/pdf/TIDUF92) ·
[Prohelion precharge sequence](https://docs.prohelion.com/Battery_Management_Systems/Prohelion_BMS_D1000_Gen1/Operation/Precharge.html) ·
[precharge failure and contactor welding](https://www.bonnenbatteries.com/ev-pre-charge-failure-troubleshooting-a-complete-guide-for-technicians/) ·
[SoC estimation methods](https://sunlithenergy.com/bms-soc-estimation/) ·
[LTC6811 isoSPI daisy chain](https://www.analog.com/media/en/technical-documentation/user-guides/dc2259af.pdf)

## Smart energy meter

The dual-core split every solid meter uses: metrology sampling on its own
tier, billing registers above, and the local load profile as the
authoritative record.

`SAMPLER` carries `ready_on_write`: the meter is ready when it is actually
metering. Client associations are a `min: 0` pool of `OnDemand` members bound
to the PLC carrier and capped at `max: 3`, the session limit a real meter
enforces with `BadTooManySessions`.

Drop the carrier and the associations bound-stop while the tariff chain and
the load profile keep running: the uplink is a convenience, the billing
record is the product. Restore it and the pool regrows on demand.

Sources:
[Microchip metrology firmware](https://onlinedocs.microchip.com/oxy/GUID-7AF360DD-C920-4EAF-93E5-FA00C76B2FC5-en-US-2/GUID-702FC4E4-88A8-4E99-9B9C-9743A1C0827D.html) ·
[Microchip smart meter SDK](https://onlinedocs.microchip.com/oxy/GUID-7AF360DD-C920-4EAF-93E5-FA00C76B2FC5-en-US-2/GUID-DA8205AC-E001-4E44-876B-5BD61813C121.html) ·
[NXP AN13338 three-phase meter design](https://www.nxp.com/docs/en/application-note/AN13338.pdf) ·
[NXP AN13742 secure metering](https://www.nxp.com/docs/en/application-note/AN13742.pdf) ·
[DLMS/COSEM and European metering standards](https://sandorian.com/energy/kb/metering-data-standards-europe)

## Robot cell controller

The dual-plane architecture real robot controllers document: non-real-time
planning, hard-RT motion, plus the safety plane that certification adds.

**All three overflow policies appear side by side**, each correct for its
direction: the planner-to-RT segment queue **back-pressures** (dropping a
segment means moving through unplanned geometry), the telemetry ring
**drops oldest** (blocking the RT tier is worse than losing a sample), and the
vision frame pool **rejects** (the camera cannot wait, and a late frame is
worthless).

`ECAT_MASTER` holds its interface as `consume` (reaching OP again means a
full bus rebuild) and must reach OP before any setpoint is written. Each
servo carries `ready_on_write`, so its first command equals its first
measured position.

Try: crash the pendant. Segments starve but the interpolator holds last-good
and setpoints keep flowing. Push the camera rate past what the frame pool
drains and it rejects; stop the logger and the telemetry ring fills in a
blink, dropping oldest. Stall a safety channel and STO asserts as a
**bound cascade**: readiness withdrawn, safety IO stops, the limit enforcer
follows, the servos stop. Restart the channel and the same edges bring the
plane back in order.

Sources:
[ROS + EtherCAT real-time control architecture](https://www.mdpi.com/2076-0825/10/7/141) ·
[ros2_control controller manager](https://control.ros.org/rolling/doc/ros2_control/controller_manager/doc/userdoc.html) ·
[per-component update rates](https://control.ros.org/master/doc/ros2_control/hardware_interface/doc/different_update_rates_userdoc.html) ·
[ethercat_driver_ros2](https://github.com/ICube-Robotics/ethercat_driver_ros2) ·
[EtherCAT multi-axis synchronization](https://www.elmomc.com/elmo_academy/ethercat-multi-axis-synchronization/) ·
[CiA 402 drive profile](https://www.can-cia.org/can-knowledge/cia-402-series-canopen-device-profile-for-drives-and-motion-control) ·
[cascaded loops and increment streams](https://www.automate.org/motion-control/industry-insights/keeping-control) ·
[safe torque off](https://novanta.com/robotics-automation/articles/safety-torque-off-explained/) ·
[EN 61800-5-2 safety sub-functions](https://download.sew-eurodrive.com/download/html/30587239/en-EN/4014183898743621600907.html) ·
[SS1/SS2/SOS explained](https://www.synapticon.com/en/motion-control-academy/sichere-stopp-funktionen-ss1-ss2-sos) ·
[LinuxCNC code notes (RT / non-RT boundary)](https://linuxcnc.org/docs/html/code/code-notes.html) ·
[Klipper code overview](https://www.klipper3d.org/Code_Overview.html) ·
[Klipper's 1024-slot move pool](https://github.com/Klipper3d/klipper/blob/master/src/basecmd.c) ·
[Klipper MCU command flow control](https://www.klipper3d.org/MCU_Commands.html) ·
[Klipper toolhead look-ahead](https://github.com/Klipper3d/klipper/blob/master/klippy/toolhead.py) ·
[grbl planner ring](https://github.com/gnea/grbl/blob/master/grbl/planner.h)

## Edge A/V streaming head

A 4K IP camera: dual-stream video, two-way audio, on-device recording and
network serving.

**The contended resource is a refcounted DMA frame pool whose teardown must
unblock its waiters before it frees.** `FRAMES` is a `Leased` signal: stopping
the allocator refuses new leases, waits for the count to reach zero, then
frees. A naive drop would deadlock; a leaked guard degrades to an ordinary
`ShutdownTimeout` naming the producer, instead of a use-after-free.

All three resource kinds are load-bearing at once: the 3A loop **lends** the
sensor I2C (borrow per frame, restored on exit); the capture pipe and encoder
channel are **consume** (a rebuild loses reference-frame state and forces a
fresh IDR); the frame pool is **shared** across every stage. Restarting the
pool is rest_for_one, and the encoder respawn *fails closed* on its spent
channel until you rebuild it.

The substream is demand-started (`OnDemand`, brought up by `start_node` when a
subscriber joins); per-client RTP sessions are a `min: 0` pool that drops its
own frames and never back-pressures the shared encoder. And `PTP_SERVO`
carries the one dependency kind nothing else in the set has: **value
freshness rather than liveness**. Stall the timestamps and the streams stay
smooth while the servo holds last-good and drifts; a heartbeat cannot catch
it.

Sources:
[GStreamer queue leaky modes](https://gstreamer.freedesktop.org/documentation/coreelements/queue.html) ·
[GStreamer buffer-pool design (deactivate-to-unblock)](https://gstreamer.freedesktop.org/documentation/additional/design/bufferpool.html) ·
[GStreamer latency design](https://gstreamer.freedesktop.org/documentation/additional/design/latency.html) ·
[appsink max-buffers](https://gstreamer.freedesktop.org/documentation/applib/gstappsink.html) ·
[rtpjitterbuffer](https://gstreamer.freedesktop.org/documentation/rtpmanager/rtpjitterbuffer.html) ·
[gst-rtsp-server media factory](https://gstreamer.freedesktop.org/documentation/gst-rtsp-server/rtsp-media-factory.html) ·
[CVITEK MPI video encoding API](https://doc.sophgo.com/cvitek-develop-docs/master/docs_latest_release/CV180x_CV181x/en/01.software/MPI/Media_Processing_Software_Development_Reference/build/html/7_Video_Encoding/API_Reference.html) ·
[CVITEK VENC design overview](https://doc.sophgo.com/cvitek-develop-docs/master/docs_latest_release/CV180x_CV181x/en/01.software/MPI/Media_Processing_Software_Development_Reference/build/html/7_Video_Encoding/Design_Overview.html) ·
[OpenIPC open Hi35xx SDK](https://github.com/OpenIPC/openhisilicon) ·
[V4L2 buffer queueing](https://docs.kernel.org/5.10/userspace-api/media/v4l/vidioc-qbuf.html) ·
[libcamera IPA (frame N stats -> N+1 params)](https://libcamera.org/api-html/classlibcamera_1_1ipa_1_1ipu3_1_1IPAIPU3.html) ·
[RFC 3550 RTP/RTCP](https://www.rfc-editor.org/rfc/rfc3550.html) ·
[RFC 4585 AVPF](https://www.rfc-editor.org/rfc/rfc4585.html) ·
[RFC 5104 CCM (PLI vs FIR)](https://www.rfc-editor.org/rfc/rfc5104.html) ·
[RFC 7273 reference clocks](https://www.rfc-editor.org/rfc/rfc7273.html) ·
[Google congestion control draft](https://datatracker.ietf.org/doc/html/draft-ietf-rmcat-gcc-02) ·
[ONVIF Media2 (SetSynchronizationPoint, encoder instances)](https://www.onvif.org/ver20/media/wsdl/media.wsdl) ·
[ONVIF recording control](https://www.onvif.org/specs/srv/rec/ONVIF-RecordingControl-Service-Spec.pdf) ·
[Axis on shared encoded streams](https://help.axis.com/en-us/troubleshooting-streaming) ·
[ALSA PCM ring, periods and XRUN](https://www.alsa-project.org/alsa-doc/alsa-lib/pcm.html) ·
[AES67 clocking](https://en.wikipedia.org/wiki/AES67) ·
[SMPTE ST 2110-10](https://pub.smpte.org/pub/st2110-10/st2110-10-2022.pdf) ·
[Dante latency guide](https://dev.audinate.com/GA/dante-controller/userguide/webhelp/content/latency.htm) ·
[Infineon ASRC (drift compensation)](https://github.com/Infineon/audio-voice-core/blob/master/docs/ASRC-README.md)

## Substation protection IED

An IEC 61850 relay across the process bus and the station bus: two networks
that fail *asymmetrically*, which makes it the clearest `ready bound`
demonstration in the set.

`SV_ALIGN` carries `ready_on_write`: protection must not evaluate before a
full aligned sample window exists, or the relay reacts to filter garbage.
`PROT_87` (differential) needs matching time sync across merging units, so it
carries `deps: [PTP_SLAVE ready bound]`: **losing PTP bound-stops
differential while overcurrent, which needs only magnitude, keeps
protecting.** Drop the station bus instead and *nothing in the protection
plane moves*: MMS stops, SCADA goes blind, the relay keeps tripping. Two link
failures, two different blast radii.

Breaker failure (`PROT_50BF`) is `OnDemand`, armed by the first trip, as a
real relay arms it. Autoreclose (`PROT_79`) is `Pause`, a state machine that
must survive across cycles. `TRIP` is a **`veto`** write from both protection
functions: a `VetoGate` where each writer holds its own contributor bit, any
bit forces the safe state and none owns it. A stopped writer's bit stays up:
trip on differential, then lose PTP, and the bound-stopped function keeps the
breaker open until it runs again and re-evaluates. Fail-safe by construction,
not by convention.

Sources:
[sampled values explained](https://scadaprotocols.com/iec-61850-sampled-values-explained/) ·
[GOOSE retransmission profile](https://help.plc.abb.com/AC500_IEC61850_resend_GOOSE_messages.html) ·
[SEL on IEC 61850 transfer-time classes](https://cdn.selinc.com/assets/Literature/Publications/Technical%20Papers/6335_IEC61850_DH-DD_20080912_Web.pdf) ·
[IEC/IEEE 61850-9-3 power profile](https://en.wikipedia.org/wiki/IEC/IEEE_61850-9-3) ·
[corrupted SV and protection blocking](https://doi.org/10.3390/en16083386) ·
[time-sync degradation in digital substations](https://tekvel.com/en/web/blog/post/time-synchronization-vulnerability-digital-substat/) ·
[GOOSE vs sampled values](https://scadaprotocols.com/iec-61850-goose-vs-sampled-values/) ·
[process / bay / station levels](https://welotec.com/en-us/blogs/knowledge/iec-61850-in-substation-automation) ·
[backup SV subscription for differential](https://link.springer.com/article/10.1186/s42162-024-00409-0) ·
[OMICRON on utility time sync](https://www.omicron-lab.com/applications/precision-timing/power-utility-time-synchronization-iec-61850)

## Multi-parameter patient monitor

The only system that **changes shape at runtime**, and the only one whose
contended resource is arbitrated by *severity rather than arrival*.

Parameter modules are `OnDemand` subtrees inserted and removed from the
device buttons. Remove one (a single stop at the acquisition) and its DSP
and alarm stages follow it down through `ready bound` edges: the absence is
*announced*, not silent. An absent module is a state the arbiter knows
about, not a fault to respawn.

The gates are hazard analysis. `PATIENT_CONTEXT` must publish before anything
runs (`ready_on_write`): adult versus neonate changes every alarm limit, so a
detector started on defaults is a real hazard. Every alarm detector carries
`deps: [ALARM_ARBITER ready]`: a detector firing into a void is a
silent-alarm hazard. The arrhythmia analyzer must complete its learning
phase before it may publish a rhythm at all.

The audio codec is a `Leased` handle the arbiter holds per alarm burst: a
truncated melody is not a conformant signal, so it cannot be re-lent
mid-burst. The NIBP pneumatics are `consume`: the measurement cycle runs to
completion and the next one fails closed until the cuff is re-armed, while
an always-on safety monitor watches regardless.

Sources:
[IEC 60601-1-8 alarm systems](https://webstore.iec.ch/en/publication/16795) ·
[PUI Audio 60601-1-8 application guide](https://puiaudio.com/resource/iec-60601-8-application-guide/) ·
[alarm priority assignment](https://www.intertek.com/medical/regulatory-requirements/iec-60601-1-8/) ·
[EN IEC 80601-2-49 multifunction monitors](https://standards.iteh.ai/catalog/standards/clc/7f173d39-86e2-4dc7-bf47-063860dddf13/en-iec-80601-2-49-2019-a1-2024) ·
[IEC 80601-2-30 NIBP overpressure cutoffs](https://webstore.iec.ch/en/publication/29812) ·
[IntelliVue plug-and-play module detection](https://documents.cdn.ifixit.com/51mPCCocSp6JQRPa.pdf) ·
[ASTM F2761 ICE architecture](https://mdpnp.org/mdice.html) ·
[OpenICE](https://mdpnp.mgh.harvard.edu/projects/openice) ·
[alarm fatigue data](https://www.ncbi.nlm.nih.gov/pmc/articles/PMC4206416/) ·
[Same Sky on 60601-1-8](https://www.sameskydevices.com/blog/a-guide-to-iec-60601-1-8-and-medical-alarm-systems) ·
[IntelliVue MMX module](https://www.usa.philips.com/healthcare/product/HC867036/philips-intellivue-mmx-multi-measurement-module) ·
[ISO/IEEE 11073-10101 nomenclature](https://www.iso.org/standard/77338.html) ·
[modular monitor docking patent](https://image-ppubs.uspto.gov/dirsearch-public/print/downloadPdf/11096631) ·
[Philips ST/AR learn and relearn phases](https://www.documents.philips.com/assets/20220223/ad31c11c4a454cbcabb7ae4501256a91.pdf) ·
[IEC 62304 safety classes](https://blog.johner-institute.com/iec-62304-medical-software/safety-class-iec-62304/)

## CubeSat flight software

The cFS shape: the best-documented flight-software roster there is. Core
services (`TIME`, `SCH`, `EVS`, `HS`, the software-bus pipe, …) around
mission apps: the ADCS chain, EPS, thermal, payload, CFDP, file manager,
stored commands.

`SCH` waits out its first major-frame sync behind a generous `slot_timeout`,
like the real SCH app waits for its 1 Hz tone. The attitude chain will not
act on a garbage quaternion: the estimator is `ready_on_write` and the
controller gates on it. The software-bus pipe **rejects** on overflow: the
router must never block on a slow subscriber.

**Demand-start is native here.** `FM`, `CF` and the payload are `OnDemand`,
started by ground command; `TO` parks *bound* until the radio comes up for a
pass. And the purest demand-start edge in the set: the limit checker's gated
read opens the sequence signal, which starts the `disabled` stored-command
app through the real control queue: a data access starting a task, with no
`deps:` on that edge. Health services answer a stalled app with the first
rung of the real ladder: an automatic restart.

Sources:
[cFE application developer's guide](https://github.com/nasa/cFE/blob/main/docs/cFE%20Application%20Developers%20Guide.md) ·
[NASA cFS project](https://opensource.gsfc.nasa.gov/projects/cfe/index.php) ·
[cFS app catalog](https://etd.gsfc.nasa.gov/capabilities/core-flight-system/catalog/) ·
[SCH platform config (100 slots, startup sync)](https://raw.githubusercontent.com/nasa/SCH/master/fsw/platform_inc/sch_platform_cfg.h) ·
[SCH readme](https://github.com/nasa/SCH/blob/master/cfs-sch-app-OSS-readme.txt) ·
[HS internal config (watchdog, restart budget)](https://raw.githubusercontent.com/nasa/HS/main/fsw/inc/hs_internal_cfg.h) ·
[HS app](https://github.com/nasa/HS) ·
[SC](https://github.com/nasa/SC) · [LC](https://github.com/nasa/LC) · [DS](https://github.com/nasa/DS) ·
[NOS3 cFS scenario](https://nos3.readthedocs.io/en/latest/Scenario_cFS.html) ·
[CubeSat flight software case study](https://pureportal.strath.ac.uk/files/250872377/Eshaq-etal-JSR-2024-CubeSat-flight-software-insights-and-a-case.pdf) ·
[NASA SWE on initialization and safe mode](https://swehb.nasa.gov/spaces/SWEHBVC/pages/140640571/Initialization+-+Safe+Mode) ·
[NASA SWE on fault detection](https://swehb.nasa.gov/display/SWEHBVC/9.07+Fault+Detection+and+Response) ·
[JPL hierarchical fault protection](https://s3vi.ndc.nasa.gov/ssri-kb/static/resources/05-2750.pdf)

## EV charging site controller

The only scenario built on a **divisible** shared resource: one site power
limit, continuously re-divided across the active charging sessions. This is
the EVerest / OCPP smart-charging shape.

When a claimant joins, every grant **shrinks instantly**; when one leaves,
grants **grow back slowly**, never in one jump. Sessions are a pool re-dividing
the budget as it scales; the RCD and thermal monitors bypass the allocator
entirely: a ground fault or an overheat is not negotiable.

The OCPP side shows the offline contract: drop the CSMS link and the
transmitter bound-stops while transaction events queue in a
**back-pressured** store-and-forward. A start precedes its meter values
precedes its stop, and that order must survive the outage. Reconnect and the
backlog drains.

The site limit is a **`divisible` resource**: `ENERGY_MGR` provides the
`Budget`, each session's `Claimant` states its want while a car is connected,
and the allocator re-divides under `ShrinkFastGrowSlow`: a cut lands at once,
an increase moves at most 4 A per period. Stop a session from the outside and
the supervisor releases its share on the shutdown ack, the worker never touches
its claim, so a dead session never strands its amps. A derate is a re-provide
with less.

Sources:
[EVerest (LF Energy)](https://lfenergy.org/projects/everest/) ·
[EVerest framework](https://everest.github.io/old-documentation-2025/general/01_framework/) ·
[everest-core](https://github.com/EVerest/everest-core) ·
[EVerest OCPP module interfaces](https://mintlify.wiki/EVerest/everest-core/modules/ocpp) ·
[EVerest energy tree as deployed](https://chargebyte-docs.readthedocs.io/projects/everest-charge-som/en/everest_charge_som_0.3.3/everest_charging_stack.html) ·
[OCPP 1.6J flows and offline ordering](https://ocpp.md/ocpp-1.6j/sequences/) ·
[OCPP 2.0.1 smart charging](https://ocpp.md/ocpp-2.0.1/smart-charging/) ·
[Open Charge Alliance](https://openchargealliance.org/protocols/open-charge-point-protocol/) ·
[AMPECO on dynamic load management](https://www.ampeco.com/dynamic-load-management/) ·
[IEC 61851-1 control pilot](https://www.einfochips.com/blog/iec-61851-everything-you-need-to-know-about-the-ev-charging-standard/) ·
[Wolfspeed on DC fast charger architecture](https://www.wolfspeed.com/knowledge-center/article/whats-under-the-hood-dc-fast-chargers-delivering-rapid-top-ups-for-evs/) ·
[J.P. Morgan store-and-forward contract](https://developer.payments.jpmorgan.com/docs/commerce/in-store-payments/capabilities/payment-terminal-application/saf)
