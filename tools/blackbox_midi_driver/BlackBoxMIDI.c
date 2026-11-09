// A CoreMIDI driver with one device, "BlackBox": the emulated ANN Blackbox as a real MIDI device.
//
// M-VAVE's M-EFCS only lists native CoreMIDI devices (a virtual endpoint has no device) with the
// name "BlackBox", and reads replies as a byte stream, so a looping port such as an IAC bus feeds
// it its own request. This driver gives it a real device with a separate source and destination.
//
// It runs inside MIDIServer and talks to `fm1-live --usb-bridge` over UDP on 127.0.0.1, one complete
// MIDI message a datagram: the driver listens on 47811, fm1-live on 47812. Nothing needs to be
// connected first: with no emulator running the device is there and silent.
//
// SPDX-License-Identifier: MIT

#include <CoreFoundation/CoreFoundation.h>
#include <CoreFoundation/CFPlugInCOM.h>
#include <CoreMIDI/CoreMIDI.h>
#include <CoreMIDI/MIDIDriver.h>
#include <arpa/inet.h>
#include <netinet/in.h>
#include <pthread.h>
#include <stdbool.h>
#include <stdlib.h>
#include <sys/socket.h>
#include <sys/time.h>
#include <unistd.h>

#define DRIVER_PORT 47811
#define EMULATOR_PORT 47812
// (macOS limits a UDP datagram to net.inet.udp.maxdgram, 9216 bytes by default)
#define MAX_MESSAGE 9000

// Info.plist's CFPlugInFactories key.
#define FACTORY_UUID                                                                               \
    CFUUIDGetConstantUUIDWithBytes(NULL, 0x7B, 0x1C, 0x3E, 0x5A, 0x4D, 0x2F, 0x4A, 0x8E, 0x9C,     \
                                   0x61, 0x3F, 0x0A, 0x8D, 0x5E, 0x2B, 0x71)

typedef struct {
    MIDIDriverInterface *vtbl; // first: a MIDIDriverRef points here
    UInt32 refs;
    MIDIEndpointRef source;
    int sock;
    volatile bool running;
    pthread_t thread;
    uint8_t sysex[MAX_MESSAGE];
    size_t sysex_len;
    bool in_sysex, sysex_overflow;
} Driver;

static Driver *of(void *self) { return (Driver *)self; }

static void to_emulator(Driver *d, const uint8_t *msg, size_t n) {
    struct sockaddr_in to = {.sin_len = sizeof to, .sin_family = AF_INET,
                             .sin_port = htons(EMULATOR_PORT), .sin_addr.s_addr = htonl(INADDR_LOOPBACK)};
    if (d->sock >= 0) sendto(d->sock, msg, n, 0, (struct sockaddr *)&to, sizeof to);
}

static size_t message_length(uint8_t status) {
    if (status < 0xf0) return (status & 0xf0) == 0xc0 || (status & 0xf0) == 0xd0 ? 2 : 3;
    if (status == 0xf1 || status == 0xf3) return 2;
    return status == 0xf2 ? 3 : 1;
}

// A packet holds whole messages or a piece of a SysEx: split them, and gather the SysEx.
static void feed(Driver *d, const uint8_t *p, size_t n) {
    for (size_t i = 0; i < n;) {
        uint8_t b = p[i];
        if (b >= 0xf8) { // real time, even inside a SysEx
            to_emulator(d, &b, 1);
            i++;
        } else if (d->in_sysex || b == 0xf0) {
            d->in_sysex = true;
            if (d->sysex_len < MAX_MESSAGE) d->sysex[d->sysex_len++] = b;
            else d->sysex_overflow = true;
            i++;
            if (b == 0xf7) {
                if (!d->sysex_overflow) to_emulator(d, d->sysex, d->sysex_len);
                d->sysex_len = 0;
                d->in_sysex = d->sysex_overflow = false;
            }
        } else if (b < 0x80) {
            i++; // a stray data byte
        } else {
            size_t len = message_length(b);
            if (len > n - i) len = n - i;
            to_emulator(d, p + i, len);
            i += len;
        }
    }
}

static void *receive_loop(void *arg) {
    Driver *d = arg;
    static uint8_t msg[MAX_MESSAGE + 216];
    static uint8_t list[sizeof msg + 1024];
    while (d->running) {
        ssize_t n = recv(d->sock, msg, sizeof msg, 0);
        if (n <= 0) continue; // (the timeout: look at `running` again)
        MIDIPacketList *pl = (MIDIPacketList *)list;
        MIDIPacket *p = MIDIPacketListInit(pl);
        p = MIDIPacketListAdd(pl, sizeof list, p, 0, (ByteCount)n, msg);
        if (p) MIDIReceived(d->source, pl);
    }
    return NULL;
}

static HRESULT QueryInterface(void *self, REFIID iid, LPVOID *out) {
    CFUUIDRef asked = CFUUIDCreateFromUUIDBytes(NULL, iid);
    bool ok = CFEqual(asked, IUnknownUUID) || CFEqual(asked, kMIDIDriverInterfaceID) ||
              CFEqual(asked, kMIDIDriverInterface2ID);
    CFRelease(asked);
    if (!ok) {
        *out = NULL;
        return E_NOINTERFACE;
    }
    of(self)->refs++;
    *out = self;
    return S_OK;
}

static ULONG AddRef(void *self) { return ++of(self)->refs; }

static ULONG Release(void *self) {
    Driver *d = of(self);
    if (--d->refs) return d->refs;
    CFPlugInRemoveInstanceForFactory(FACTORY_UUID);
    free(d);
    return 0;
}

static OSStatus FindDevices(MIDIDriverRef self, MIDIDeviceListRef devices) { return noErr; }

static OSStatus Start(MIDIDriverRef self, MIDIDeviceListRef devices) {
    Driver *d = of(self);
    // the device from an earlier run stays in the MIDI setup: reuse it
    MIDIDeviceListRef mine = MIDIGetDriverDeviceList(self);
    MIDIDeviceRef device = MIDIDeviceListGetNumberOfDevices(mine) ? MIDIDeviceListGetDevice(mine, 0) : 0;
    MIDIDeviceListDispose(mine);
    MIDIEntityRef entity = 0;
    if (device) {
        entity = MIDIDeviceGetEntity(device, 0);
    } else {
        // (device and entity share the name: the endpoint's display name is then just "BlackBox")
        OSStatus err = MIDIDeviceCreate(self, CFSTR("BlackBox"), CFSTR("M-VAVE"), CFSTR("BlackBox"), &device);
        if (err) return err;
        err = MIDIDeviceAddEntity(device, CFSTR("BlackBox"), false, 1, 1, &entity);
        if (err) return err;
        err = MIDISetupAddDevice(device);
        if (err) return err;
    }
    d->source = MIDIEntityGetSource(entity, 0);

    d->sock = socket(AF_INET, SOCK_DGRAM, 0);
    if (d->sock < 0) return kMIDINoConnection;
    struct sockaddr_in at = {.sin_len = sizeof at, .sin_family = AF_INET,
                             .sin_port = htons(DRIVER_PORT), .sin_addr.s_addr = htonl(INADDR_LOOPBACK)};
    struct timeval wait = {.tv_usec = 200000};
    setsockopt(d->sock, SOL_SOCKET, SO_RCVTIMEO, &wait, sizeof wait);
    if (bind(d->sock, (struct sockaddr *)&at, sizeof at)) {
        close(d->sock);
        d->sock = -1;
        return kMIDINoConnection;
    }
    d->running = true;
    pthread_create(&d->thread, NULL, receive_loop, d);
    return noErr;
}

static OSStatus Stop(MIDIDriverRef self) {
    Driver *d = of(self);
    if (d->running) {
        d->running = false;
        pthread_join(d->thread, NULL);
    }
    if (d->sock >= 0) close(d->sock);
    d->sock = -1;
    return noErr;
}

static OSStatus Configure(MIDIDriverRef self, MIDIDeviceRef device) { return noErr; }

static OSStatus Send(MIDIDriverRef self, const MIDIPacketList *list, void *ref1, void *ref2) {
    const MIDIPacket *p = &list->packet[0];
    for (UInt32 i = 0; i < list->numPackets; i++) {
        feed(of(self), p->data, p->length);
        p = MIDIPacketNext(p);
    }
    return noErr;
}

static OSStatus EnableSource(MIDIDriverRef self, MIDIEndpointRef src, Boolean on) { return noErr; }
static OSStatus Flush(MIDIDriverRef self, MIDIEndpointRef dest, void *ref1, void *ref2) { return noErr; }
static OSStatus Monitor(MIDIDriverRef self, MIDIEndpointRef dest, const MIDIPacketList *list) { return noErr; }

static MIDIDriverInterface vtable = {
    .QueryInterface = QueryInterface,
    .AddRef = AddRef,
    .Release = Release,
    .FindDevices = FindDevices,
    .Start = Start,
    .Stop = Stop,
    .Configure = Configure,
    .Send = Send,
    .EnableSource = EnableSource,
    .Flush = Flush,
    .Monitor = Monitor,
};

__attribute__((visibility("default")))
void *NewBlackBoxDriver(CFAllocatorRef allocator, CFUUIDRef type) {
    if (!CFEqual(type, kMIDIDriverTypeID)) return NULL;
    Driver *d = calloc(1, sizeof *d);
    d->vtbl = &vtable;
    d->refs = 1;
    d->sock = -1;
    CFPlugInAddInstanceForFactory(FACTORY_UUID);
    return d;
}
