# IOMMU/DMA-BUF Investigation for OnePlus 6 (SDM845)

## Problem Statement

The `dma-buf-overhaul` branch causes kernel hangs and SMMU faults when running on the OnePlus 6 (Qualcomm SDM845) running postmarketOS.

## Symptoms

```
DMA-BUF fence timeout after 5005ms (fd 9)

Kernel logs:
arm-smmu 15000000.iommu: Unhandled context fault: fsr=0x408, iova=0xfff000000, fsynr=0x160013, cbfrsynra=0x740, cb=9
arm-smmu 15000000.iommu: FSR    = 00000408 [Format=2 PF], SID=0x740
arm-smmu 15000000.iommu: FSYNR0 = 00160013 [S1CBNDX=22 WNR PLVL=3]
dwc3 a600000.usb: end transfer failed: -110
```

## Technical Analysis

### Hardware Configuration

- **SoC**: Qualcomm Snapdragon 845 (SDM845)
- **Device**: OnePlus 6 (enchilada)
- **OS**: postmarketOS edge
- **Kernel**: 6.16.7-sdm845

### IOMMU Groups

```
Group 8:  ae00000.display-subsystem (display, qcom,sdm845-mdss)
Group 12: a600000.usb (USB controller, snps,dwc3)
```

### Kernel Configuration

DMA-BUF related options (all enabled):
```
CONFIG_DMA_SHARED_BUFFER=y
CONFIG_DMABUF_HEAPS=y
CONFIG_DMABUF_HEAPS_SYSTEM=y
CONFIG_DMABUF_HEAPS_CMA=y
```

FunctionFS options:
```
CONFIG_USB_F_FS=y
CONFIG_USB_CONFIGFS_F_FS=y
```

IOMMU options:
```
CONFIG_IOMMU_SUPPORT=y
CONFIG_IOMMU_DMA=y
CONFIG_ARM_SMMU=y
CONFIG_ARM_SMMU_QCOM=y
```

### DMA Heaps Available

```
/dev/dma_heap/system
/dev/dma_heap/reserved
```

### Error Analysis

The SMMU fault shows:
- `iova=0xfff000000` - Invalid DMA address (looks like error code or unmapped address)
- `SID=0x740` - USB controller Stream ID
- `S1CBNDX=22` - Context bank 22
- `WNR=1` - Write Not Read (write access caused fault)

### Root Cause

The DMA-BUF allocated from the system heap is:
1. Successfully allocated (mmap works)
2. Successfully attached to FunctionFS endpoint (ioctl returns 0)
3. **NOT properly mapped into the USB controller's IOMMU domain**
4. When USB controller tries to DMA to the buffer, SMMU blocks access

This is a **kernel-level bug** in the interaction between:
- FunctionFS DMA-BUF implementation (`drivers/usb/gadget/function/f_fs.c`)
- Qualcomm SMMU driver (`drivers/iommu/arm/arm-smmu/arm-smmu-qcom.c`)

### Why This Works on Other Platforms

FunctionFS DMA-BUF support was added in Linux 6.7. The implementation assumes:
1. DMA-BUF attachment automatically sets up IOMMU mappings
2. The device driver (FunctionFS) properly integrates with DMA-BUF framework

On SDM845 with the Qualcomm SMMU, this integration appears to be broken for the USB controller.

## Alternative Approaches Tested

| Approach | Result |
|----------|--------|
| Main branch (regular reads) | Works, but display stretched |
| DMA-BUF from system heap | SMMU fault, timeout |
| DMA-BUF from reserved heap | Not tested |

## Code Flow Analysis

### DMA-BUF Flow (dma-buf-overhaul branch)

```
1. Heap::new(HeapKind::System)
   → Allocates DMA-BUF from system heap
   
2. ffs_dmabuf_attach(ep_fd, &buffer_fd)
   → Attaches DMA-BUF to FunctionFS endpoint
   → Returns 0 (success)
   
3. ffs_dmabuf_transfer(ep_fd, &req)
   → Submits transfer request
   → Returns 0 (success)
   
4. dma_buf_export_sync_file()
   → Exports sync file for fence
   → Returns fd (success)
   
5. poll(sync_fd, POLLIN, timeout=5000ms)
   → Waits for transfer completion
   → TIMEOUT after 5000ms
   → Meanwhile: SMMU fault in kernel
```

### The Missing Step

Between steps 2 and 3, the DMA-BUF should be mapped into the USB controller's IOMMU domain. This mapping is either:
- Not happening at all
- Happening with incorrect parameters
- Being bypassed by an error path

## Potential Fixes

### Option 1: Kernel Patch (Hard)

Fix the FunctionFS DMA-BUF + Qualcomm SMMU integration in the kernel.

**Pros**: Proper fix, benefits all users
**Cons**: Requires kernel development, may not be accepted upstream

### Option 2: Different DMA Heap (Medium)

Try allocating from `/dev/dma_heap/reserved` instead of `system`.

**Pros**: May work around IOMMU issues
**Cons**: Reserved heap may have limited memory, not guaranteed to work

### Option 3: Fallback to Regular Reads (Easy)

When DMA-BUF fails/times out, fall back to synchronous `read()` calls.

**Pros**: Simple, guaranteed to work
**Cons**: Lose zero-copy benefits, extra memory copy

### Option 4: Fix Main Branch (Easy)

The main branch works but has stretched display. This is likely a mode negotiation or pixel format issue, not DMA-related.

**Pros**: Already mostly working
**Cons**: No zero-copy optimization

## Related Issues

- [usb-gadget #19: FunctionFS DMA-BUF support](https://github.com/surban/usb-gadget/issues/19)
- [Linux kernel FunctionFS DMA-BUF patches](https://patchew.org/linux/20240108120056.22165-1-paul@crapouillou.net/)

## Conclusion

The DMA-BUF issue is a kernel-level bug specific to the interaction between FunctionFS and Qualcomm SMMU on SDM845. For the userspace driver, the pragmatic solutions are:

1. **Add fallback to regular reads** when DMA-BUF fails
2. **Fix the main branch** display stretching issue instead

Both approaches will result in a working driver, with option 2 being simpler since the main branch already works with regular reads.
