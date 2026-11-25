use windows::{
    Win32::{
        Foundation::E_FAIL,
        Graphics::Direct3D11::{
            D3D11_CPU_ACCESS_READ, D3D11_MAP_READ, D3D11_MAPPED_SUBRESOURCE, D3D11_TEXTURE2D_DESC,
            D3D11_USAGE_STAGING, ID3D11Texture2D,
        },
    },
    core::{Error, Result},
};

#[derive(Default)]
pub struct Readback {
    staging: Option<ID3D11Texture2D>,
    extent: (u32, u32),
}

impl Readback {
    pub fn copy(
        &mut self,
        texture: &ID3D11Texture2D,
        width: u32,
        height: u32,
        pixels: &mut Vec<u8>,
    ) -> Result<()> {
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        unsafe { texture.GetDesc(&mut desc) };
        if width > desc.Width || height > desc.Height {
            return Err(Error::new(E_FAIL, "Capture extent exceeds texture"));
        }
        let device = unsafe { texture.GetDevice()? };
        if self.staging.is_none() || self.extent != (desc.Width, desc.Height) {
            desc.Usage = D3D11_USAGE_STAGING;
            desc.BindFlags = 0;
            desc.MiscFlags = 0;
            desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
            let mut staging = None;
            unsafe { device.CreateTexture2D(&desc, None, Some(&mut staging))? };
            self.staging = staging;
            self.extent = (desc.Width, desc.Height);
        }
        let staging = self
            .staging
            .as_ref()
            .ok_or_else(|| Error::new(E_FAIL, "Missing staging texture"))?;
        let context = unsafe { device.GetImmediateContext()? };
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        let stride = width as usize * 4;
        pixels.resize(stride * height as usize, 0);
        unsafe {
            context.CopyResource(staging, texture);
            context.Map(staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;
            if (mapped.RowPitch as usize) < stride || mapped.pData.is_null() {
                context.Unmap(staging, 0);
                return Err(Error::new(E_FAIL, "Invalid mapped capture texture"));
            }
            for (row, target) in pixels.chunks_exact_mut(stride).enumerate() {
                let source = mapped
                    .pData
                    .cast::<u8>()
                    .add(row * mapped.RowPitch as usize);
                std::ptr::copy_nonoverlapping(source, target.as_mut_ptr(), stride);
            }
            context.Unmap(staging, 0);
        }
        Ok(())
    }
}
