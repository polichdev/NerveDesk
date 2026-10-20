#include "Driver.h"

extern "C" DRIVER_INITIALIZE DriverEntry;

EVT_WDF_DRIVER_DEVICE_ADD EvtDeviceAdd;
EVT_WDF_DEVICE_D0_ENTRY EvtDeviceD0Entry;
EVT_WDF_DEVICE_D0_EXIT EvtDeviceD0Exit;

EVT_IDD_CX_ADAPTER_INIT_FINISHED EvtAdapterInitFinished;
EVT_IDD_CX_ADAPTER_COMMIT_MODES EvtAdapterCommitModes;
EVT_IDD_CX_PARSE_MONITOR_DESCRIPTION EvtParseMonitorDescription;
EVT_IDD_CX_MONITOR_GET_DEFAULT_DESCRIPTION_MODES EvtMonitorGetDefaultModes;
EVT_IDD_CX_MONITOR_QUERY_TARGET_MODES EvtMonitorQueryTargetModes;
EVT_IDD_CX_MONITOR_ASSIGN_SWAPCHAIN EvtMonitorAssignSwapChain;
EVT_IDD_CX_MONITOR_UNASSIGN_SWAPCHAIN EvtMonitorUnassignSwapChain;

extern "C" NTSTATUS DriverEntry(
    _In_ PDRIVER_OBJECT DriverObject,
    _In_ PUNICODE_STRING RegistryPath)
{
    WDF_DRIVER_CONFIG config;
    WDF_DRIVER_CONFIG_INIT(&config, EvtDeviceAdd);

    NTSTATUS status = WdfDriverCreate(
        DriverObject,
        RegistryPath,
        WDF_NO_OBJECT_ATTRIBUTES,
        &config,
        WDF_NO_HANDLE);

    return status;
}

NTSTATUS EvtDeviceAdd(
    _In_ WDFDRIVER Driver,
    _Inout_ PWDFDEVICE_INIT DeviceInit)
{
    UNREFERENCED_PARAMETER(Driver);

    NTSTATUS status = IddCxDeviceInitConfig(DeviceInit, nullptr);
    if (!NT_SUCCESS(status))
    {
        return status;
    }

    WDF_OBJECT_ATTRIBUTES attr;
    WDF_OBJECT_ATTRIBUTES_INIT_CONTEXT_TYPE(&attr, IndirectDeviceContext);

    WDFDEVICE device;
    status = WdfDeviceCreate(&DeviceInit, &attr, &device);
    if (!NT_SUCCESS(status))
    {
        return status;
    }

    IndirectDeviceContext* pContext = IndirectDeviceContext::GetContext(device);
    pContext->WdfDevice = device;

    IDARG_IN_ADAPTER_INIT adapterInit = {};
    adapterInit.WdfDevice = device;
    adapterInit.pfnAdapterInitFinished = EvtAdapterInitFinished;
    adapterInit.pfnAdapterCommitModes = EvtAdapterCommitModes;

    IDARG_OUT_ADAPTER_INIT adapterOut;
    status = IddCxAdapterInit(&adapterInit, &adapterOut);
    if (!NT_SUCCESS(status))
    {
        return status;
    }

    pContext->IddAdapter = adapterOut.AdapterObject;

    return STATUS_SUCCESS;
}

NTSTATUS EvtAdapterInitFinished(
    _In_ IDDCX_ADAPTER AdapterObject,
    _In_ const IDARG_IN_ADAPTER_INIT_FINISHED* pInArgs)
{
    UNREFERENCED_PARAMETER(AdapterObject);
    UNREFERENCED_PARAMETER(pInArgs);

    WDFDEVICE device = IddCxAdapterGetWdfDevice(AdapterObject);
    IndirectDeviceContext* pContext = IndirectDeviceContext::GetContext(device);

    IDDCX_MONITOR_INFO monitorInfo = {};
    monitorInfo.Size = sizeof(monitorInfo);
    monitorInfo.MonitorType = DISPLAYCONFIG_OUTPUT_TECHNOLOGY_DISPLAYPORT_EMBEDDED;
    monitorInfo.ConnectorIndex = 0;

    monitorInfo.MonitorDescription.Size = sizeof(monitorInfo.MonitorDescription);
    monitorInfo.MonitorDescription.Type = IDDCX_MONITOR_DESCRIPTION_TYPE_EDID;
    monitorInfo.MonitorDescription.DataSize = sizeof(NerveDesk::s_KnownMonitorEdid);
    monitorInfo.MonitorDescription.pData = const_cast<BYTE*>(NerveDesk::s_KnownMonitorEdid);

    IDARG_IN_MONITORCREATE monitorCreate = {};
    monitorCreate.ObjectAttributes.Size = sizeof(monitorCreate.ObjectAttributes);
    monitorCreate.pMonitorInfo = &monitorInfo;

    IDARG_OUT_MONITORCREATE monitorCreateOut;
    NTSTATUS status = IddCxMonitorCreate(AdapterObject, &monitorCreate, &monitorCreateOut);
    if (!NT_SUCCESS(status))
    {
        return status;
    }

    pContext->IddMonitor = monitorCreateOut.MonitorObject;

    IDARG_IN_MONITORARRIVAL arrivalArgs = {};
    arrivalArgs.Size = sizeof(arrivalArgs);
    status = IddCxMonitorArrival(pContext->IddMonitor, &arrivalArgs);

    return status;
}

NTSTATUS EvtAdapterCommitModes(
    _In_ IDDCX_ADAPTER AdapterObject,
    _In_ const IDARG_IN_COMMITMODES* pInArgs)
{
    UNREFERENCED_PARAMETER(AdapterObject);
    UNREFERENCED_PARAMETER(pInArgs);
    return STATUS_SUCCESS;
}

NTSTATUS EvtParseMonitorDescription(
    _In_ const IDARG_IN_PARSEMONITORDESCRIPTION* pInArgs,
    _Out_ IDARG_OUT_PARSEMONITORDESCRIPTION* pOutArgs)
{
    UNREFERENCED_PARAMETER(pInArgs);
    UNREFERENCED_PARAMETER(pOutArgs);
    return STATUS_SUCCESS;
}

NTSTATUS EvtMonitorGetDefaultModes(
    _In_ IDDCX_MONITOR MonitorObject,
    _In_ const IDARG_IN_GETDEFAULTDESCRIPTIONMODES* pInArgs,
    _Out_ IDARG_OUT_GETDEFAULTDESCRIPTIONMODES* pOutArgs)
{
    UNREFERENCED_PARAMETER(MonitorObject);
    UNREFERENCED_PARAMETER(pInArgs);
    UNREFERENCED_PARAMETER(pOutArgs);
    return STATUS_SUCCESS;
}

NTSTATUS EvtMonitorQueryTargetModes(
    _In_ IDDCX_MONITOR MonitorObject,
    _In_ const IDARG_IN_QUERYTARGETMODES* pInArgs,
    _Out_ IDARG_OUT_QUERYTARGETMODES* pOutArgs)
{
    UNREFERENCED_PARAMETER(MonitorObject);
    UNREFERENCED_PARAMETER(pInArgs);
    UNREFERENCED_PARAMETER(pOutArgs);
    return STATUS_SUCCESS;
}

NTSTATUS EvtMonitorAssignSwapChain(
    _In_ IDDCX_MONITOR MonitorObject,
    _In_ const IDARG_IN_SETSWAPCHAIN* pInArgs)
{
    UNREFERENCED_PARAMETER(MonitorObject);
    UNREFERENCED_PARAMETER(pInArgs);
    return STATUS_SUCCESS;
}

NTSTATUS EvtMonitorUnassignSwapChain(
    _In_ IDDCX_MONITOR MonitorObject)
{
    UNREFERENCED_PARAMETER(MonitorObject);
    return STATUS_SUCCESS;
}
