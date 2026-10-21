#pragma once

#include <windows.h>
#include <wdf.h>
#include <iddcx.h>
#include <d3d11.h>
#include <dxgi1_4.h>
#include <wrl/client.h>

struct StreamWorker;

struct DeviceContext {
    WDFDEVICE device;
    IDDCX_ADAPTER adapter;
};

struct MonitorContext {
    StreamWorker* worker;
};

WDF_DECLARE_CONTEXT_TYPE_WITH_NAME(DeviceContext, GetDeviceContext);
WDF_DECLARE_CONTEXT_TYPE_WITH_NAME(MonitorContext, GetMonitorContext);
