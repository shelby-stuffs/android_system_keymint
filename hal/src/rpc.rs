//! RemotelyProvisionedComponent HAL device implementation.

use super::{ChannelHalService, SerializedChannel};
use crate::binder;
use crate::hal::{keymint, Innto};
use kmr_wire::*;
use std::sync::{Arc, Mutex, MutexGuard};

/// `IRemotelyProvisionedComponent` implementation which converts all method invocations to
/// serialized requests that are sent down the associated channel.
pub struct Device<T: SerializedChannel + 'static> {
    channel: Arc<Mutex<T>>,
}

impl<T: SerializedChannel + 'static> Device<T> {
    /// Construct a new instance that uses the provided channel.
    pub fn new(channel: Arc<Mutex<T>>) -> Self {
        Self { channel }
    }

    /// Create a new instance wrapped in a proxy object.
    pub fn new_as_binder(
        channel: Arc<Mutex<T>>,
    ) -> binder::Strong<dyn keymint::IRemotelyProvisionedComponent::IRemotelyProvisionedComponent>
    {
        keymint::IRemotelyProvisionedComponent::BnRemotelyProvisionedComponent::new_binder(
            Self::new(channel),
            binder::BinderFeatures::default(),
        )
    }
}

impl<T: SerializedChannel> ChannelHalService<T> for Device<T> {
    fn channel(&self) -> MutexGuard<T> {
        self.channel.lock().unwrap()
    }
}

impl<T: SerializedChannel> binder::Interface for Device<T> {}

impl<T: SerializedChannel> keymint::IRemotelyProvisionedComponent::IRemotelyProvisionedComponent
    for Device<T>
{
    fn getHardwareInfo(&self) -> binder::Result<keymint::RpcHardwareInfo::RpcHardwareInfo> {
        let rsp: GetRpcHardwareInfoResponse = self.execute(GetRpcHardwareInfoRequest {})?;
        Ok(rsp.ret.innto())
    }
    fn generateEcdsaP256KeyPair(
        &self,
        testMode: bool,
        macedPublicKey: &mut keymint::MacedPublicKey::MacedPublicKey,
    ) -> binder::Result<Vec<u8>> {
        let rsp: GenerateEcdsaP256KeyPairResponse =
            self.execute(GenerateEcdsaP256KeyPairRequest { test_mode: testMode })?;
        *macedPublicKey = rsp.maced_public_key.innto();
        Ok(rsp.ret)
    }
    fn generateCertificateRequest(
        &self,
        testMode: bool,
        keysToSign: &[keymint::MacedPublicKey::MacedPublicKey],
        endpointEncryptionCertChain: &[u8],
        challenge: &[u8],
        deviceInfo: &mut keymint::DeviceInfo::DeviceInfo,
        protectedData: &mut keymint::ProtectedData::ProtectedData,
    ) -> binder::Result<Vec<u8>> {
        let rsp: GenerateCertificateRequestResponse =
            self.execute(GenerateCertificateRequestRequest {
                test_mode: testMode,
                keys_to_sign: keysToSign.iter().map(|k| k.innto()).collect(),
                endpoint_encryption_cert_chain: endpointEncryptionCertChain.to_vec(),
                challenge: challenge.to_vec(),
            })?;
        *deviceInfo = rsp.device_info.innto();
        *protectedData = rsp.protected_data.innto();
        Ok(rsp.ret)
    }
    fn generateCertificateRequestV2(
        &self,
        keysToSign: &[keymint::MacedPublicKey::MacedPublicKey],
        challenge: &[u8],
    ) -> binder::Result<Vec<u8>> {
        let rsp: GenerateCertificateRequestV2Response =
            self.execute(GenerateCertificateRequestV2Request {
                keys_to_sign: keysToSign.iter().map(|k| k.innto()).collect(),
                challenge: challenge.to_vec(),
            })?;
        Ok(rsp.ret)
    }
}
