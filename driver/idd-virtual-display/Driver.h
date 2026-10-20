#pragma once

#include <windows.h>
#include <bugcodes.h>
#include <wdf.h>
#include <iddcx.h>

#include <dxgi1_5.h>
#include <d3d11_2.h>
#include <wrl.h>

#include "Edid.h"

using Microsoft::WRL::ComPtr;

DEFINE_GUID(GUID_NERVEDESK_IDD_TRACE, 
    0x7e6a93f5, 0x1994, 0x4ac6, 0xb0, 0xb1, 0x4d, 0xf5, 0x63, 0x40, 0xb2, 0xd4);

struct IndirectDeviceContext
{
    WDFDEVICE WdfDevice;
    IDDCX_ADAPTER IddAdapter;
    IDDCX_MONITOR IddMonitor;

    ComPtr<ID3D11Device> D3DDevice;
    ComPtr<ID3D11DeviceContext> D3DContext;

    static IndirectDeviceContext* GetContext(WDFDEVICE device);
};

WDF_DECLARE_CONTEXT_TYPE_WITH_NAME(IndirectDeviceContext, GetIndirectDeviceContext)

inline IndirectDeviceContext* IndirectDeviceContext::GetContext(WDFDEVICE device)
{
    return GetIndirectDeviceContext(device);
}
