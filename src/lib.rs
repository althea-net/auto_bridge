use clarity::abi::encode_call;
use clarity::{Address, PrivateKey};
use failure::{ensure, Error};
use futures::Future;
use num256::Uint256;
use std::str::FromStr;
use web30::client::Web3;
use web30::types::Log;

#[derive(Clone)]
pub struct TokenBridge {
    xdai_web3: Web3,
    eth_web3: Web3,
    uniswap_address: Address,
    /// This is the address of the xDai bridge on Eth
    xdai_foreign_bridge_address: Address,
    /// This is the address of the xDai bridge on xDai
    xdai_home_bridge_address: Address,
    /// This is the address of the Dai token contract on Eth
    foreign_dai_contract_address: Address,
    own_address: Address,
    secret: PrivateKey,
}

impl TokenBridge {
    pub fn new(
        uniswap_address: Address,
        xdai_home_bridge_address: Address,
        xdai_foreign_bridge_address: Address,
        foreign_dai_contract_address: Address,
        own_address: Address,
        secret: PrivateKey,
        eth_full_node_url: String,
        xdai_full_node_url: String,
    ) -> TokenBridge {
        TokenBridge {
            uniswap_address,
            xdai_home_bridge_address,
            xdai_foreign_bridge_address,
            foreign_dai_contract_address,
            own_address,
            secret,
            xdai_web3: Web3::new(&xdai_full_node_url),
            eth_web3: Web3::new(&eth_full_node_url),
        }
    }

    /// Price of ETH in Dai
    fn eth_to_dai_price(&self, amount: Uint256) -> Box<Future<Item = Uint256, Error = Error>> {
        let web3 = self.eth_web3.clone();
        let uniswap_address = self.uniswap_address.clone();
        let own_address = self.own_address.clone();

        let props = web3
            .eth_get_balance(uniswap_address)
            .join(web3.contract_call(
                uniswap_address,
                "balanceOf(address)",
                &[own_address.into()],
                own_address,
            ));

        Box::new(props.and_then(move |(input_reserve, output_reserve)| {
            let numerator = amount.clone() * output_reserve * 997u64.into();
            let denominator = input_reserve * 1000u64.into() + amount * 997u64.into();
            Ok(numerator / denominator)
        }))
    }

    /// Price of Dai in ETH
    fn dai_to_eth_price(&self, amount: Uint256) -> Box<Future<Item = Uint256, Error = Error>> {
        let web3 = self.eth_web3.clone();
        let uniswap_address = self.uniswap_address.clone();
        let own_address = self.own_address.clone();

        let props = web3
            .eth_get_balance(uniswap_address)
            .join(web3.contract_call(
                uniswap_address,
                "balanceOf(address)",
                &[own_address.into()],
                own_address,
            ));

        Box::new(props.and_then(move |(output_reserve, input_reserve)| {
            let numerator = amount.clone() * output_reserve * 997u64.into();
            let denominator = input_reserve * 1000u64.into() + amount * 997u64.into();
            Ok(numerator / denominator)
        }))
    }

    /// Sell `eth_amount` ETH for Dai
    fn eth_to_dai_swap(&self, eth_amount: Uint256) -> Box<Future<Item = Uint256, Error = Error>> {
        let uniswap_address = self.uniswap_address.clone();
        let own_address = self.own_address.clone();
        let secret = self.secret.clone();
        let web3 = self.eth_web3.clone();
        Box::new(
            web3.eth_block_number()
                .and_then({
                    let web3 = web3.clone();
                    move |current_block_number| web3.eth_get_block_by_number(current_block_number)
                })
                .join(self.eth_to_dai_price(eth_amount.clone()))
                .and_then(move |(block, expected_dai)| {
                    // Equivalent to `amount * (1 - 0.025)` without using decimals
                    let expected_dai = (expected_dai / 40u64.into()) * 39u64.into();
                    let payload = encode_call(
                        "ethToTokenSwapInput(uint256,uint256)",
                        &[expected_dai.clone().into(), block.timestamp.into()],
                    );

                    // Box::new(
                    web3.send_transaction(uniswap_address, payload, eth_amount, own_address, secret)
                        .join(web3.wait_for_event(
                            uniswap_address,
                            "TokenPurchase(address,uint256,uint256)",
                            Some(vec![own_address.into()]),
                            None,
                            None,
                            |_| true,
                        ))
                        .and_then(move |(_tx, response)| {
                            let transfered_dai = Uint256::from_bytes_le(&response.topics[3]);
                            ensure!(
                                transfered_dai == expected_dai,
                                "Transfered dai is not equal to expected dai"
                            );
                            Ok(transfered_dai)
                        })
                }),
        )
    }

    /// Sell `dai_amount` Dai for ETH
    fn dai_to_eth_swap(&self, dai_amount: Uint256) -> Box<Future<Item = Uint256, Error = Error>> {
        let uniswap_address = self.uniswap_address.clone();
        let own_address = self.own_address.clone();
        let secret = self.secret.clone();
        let web3 = self.eth_web3.clone();

        Box::new(
            web3.eth_block_number()
                .and_then({
                    let web3 = web3.clone();
                    move |current_block| web3.eth_get_block_by_number(current_block)
                })
                .join(self.dai_to_eth_price(dai_amount.clone()))
                .and_then(move |(block, expected_eth)| {
                    // Equivalent to `amount * (1 - 0.025)` without using decimals
                    let expected_eth = (expected_eth / 40u64.into()) * 39u64.into();

                    Box::new(
                        web3.send_transaction(
                            uniswap_address,
                            encode_call(
                                "tokenToEthSwapInput(uint256,uint256,uint256)",
                                &[
                                    dai_amount.into(),
                                    expected_eth.clone().into(),
                                    block.timestamp.into(),
                                ],
                            ),
                            0u32.into(),
                            own_address,
                            secret,
                        )
                        .join(web3.wait_for_event(
                            uniswap_address,
                            "EthPurchase(address,uint256,uint256)",
                            Some(vec![own_address.into()]),
                            None,
                            None,
                            |_| true,
                        ))
                        .and_then(move |(_tx, response)| {
                            // let mut data: [u8; 32] = Default::default();
                            // data.copy_from_slice(&response.data);
                            // Ok(data.into())
                            let transfered_eth = Uint256::from_bytes_le(&response.topics[3]);
                            ensure!(
                                transfered_eth == expected_eth,
                                "Transfered eth is not equal to expected eth"
                            );
                            Ok(transfered_eth)
                        }),
                    )
                }),
        )
    }

    /// Convert `dai_amount` dai to xdai. This doesn't currently let you know when the transfer is done.
    /// In most use cases, it should be ok to just let the user notice when their balance has increased.
    /// The xDai chain seems to transfer tokens from the bridge by giving the receiving account a block
    /// reward so there is no transfer event. There are events that are fired for but for some crazy reason,
    /// they decided not to make these events indexed so they cannot be listened for:
    /// https://forum.poa.network/t/how-to-relay-dai-stablecoins-without-usage-of-the-bridge-ui/1876/5?u=jtremback
    ///
    /// Not sure how valuable it is for us to fix their bridge since we will be using Cosmos soon anyway.
    /// Making the events indexed would be a breaking change since it would change the API. Getting
    /// a PR with the indexed events merged and deployed could be a struggle. Another option would be
    /// to index the unindexed properties ourselves with something like The Graph.

    fn dai_to_xdai_bridge(
        &self,
        dai_amount: Uint256,
    ) -> Box<Future<Item = Uint256, Error = Error>> {
        let eth_web3 = self.eth_web3.clone();
        let xdai_home_bridge_address = self.xdai_home_bridge_address.clone();
        let xdai_foreign_bridge_address = self.xdai_home_bridge_address.clone();
        let own_address = self.own_address.clone();
        let secret = self.secret.clone();

        // You basically just send it some coins
        Box::new(eth_web3.send_transaction(
            xdai_home_bridge_address,
            encode_call(
                "transfer(address,uint256)",
                &[
                    xdai_foreign_bridge_address.into(),
                    dai_amount.clone().into(),
                ],
            ),
            0u32.into(),
            own_address,
            secret,
        ))
    }

    /// Convert `xdai_amount` xdai to dai
    /// As part of the conversion the amount to be sent is "tagged" by adding a tiny amount to it,
    /// up to 65 kwei
    fn xdai_to_dai_bridge(&self, xdai_amount: Uint256) -> Box<Future<Item = Log, Error = Error>> {
        let xdai_web3 = self.xdai_web3.clone();
        let eth_web3 = self.eth_web3.clone();
        let xdai_home_bridge_address = self.xdai_home_bridge_address.clone();
        let xdai_foreign_bridge_address = self.xdai_home_bridge_address.clone();
        let foreign_dai_contract_address = self.foreign_dai_contract_address.clone();
        let own_address = self.own_address.clone();
        let secret = self.secret.clone();

        // We tag the amount with a small random number so that we can identify it later
        let tagged_amount: Uint256 = xdai_amount + rand::random::<u16>().into();

        // You basically just send it some coins
        Box::new(
            xdai_web3
                .send_transaction(
                    xdai_home_bridge_address,
                    [].to_vec(),
                    tagged_amount.clone(),
                    own_address,
                    secret,
                )
                .join(eth_web3.wait_for_event(
                    foreign_dai_contract_address,
                    "Transfer(address,address,uint256)",
                    Some(vec![xdai_foreign_bridge_address.into()]),
                    Some(vec![own_address.into()]),
                    None,
                    move |log| Uint256::from_bytes_be(&log.data) == tagged_amount,
                ))
                .and_then(|(_, log)| futures::future::ok(log)),
        )
    }

    pub fn convert_eth_to_xdai(
        &self,
        eth_amount: Uint256,
    ) -> Box<Future<Item = Uint256, Error = Error>> {
        let salf = self.clone();

        Box::new(
            salf.eth_to_dai_swap(eth_amount)
                .and_then(move |dai_amount| salf.dai_to_xdai_bridge(dai_amount)),
        )
    }

    pub fn convert_xdai_to_eth(
        &self,
        xdai_amount: Uint256,
    ) -> Box<Future<Item = Uint256, Error = Error>> {
        let salf = self.clone();

        Box::new(
            salf.xdai_to_dai_bridge(xdai_amount.clone())
                .and_then(move |_| salf.dai_to_eth_swap(xdai_amount)),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use actix;

    fn new_token_bridge() -> TokenBridge {
        TokenBridge::new(
            Address::from_str("0x09cabEC1eAd1c0Ba254B09efb3EE13841712bE14".into()).unwrap(),
            Address::from_str("0x7301CFA0e1756B71869E93d4e4Dca5c7d0eb0AA6".into()).unwrap(),
            Address::from_str("0x4aa42145Aa6Ebf72e164C9bBC74fbD3788045016".into()).unwrap(),
            Address::from_str("0x89d24A6b4CcB1B6fAA2625fE562bDD9a23260359".into()).unwrap(),
            Address::from_str("0x46efca97bCD20544616D6Df1724628b5b26eB413".into()).unwrap(),
            PrivateKey::from_str(
                "1F804A16150F4C0E1EB966A9BAE9683FF4E760EF189BA98C98477081C334E123".into(),
            )
            .unwrap(),
            "https://mainnet.infura.io/v3/4bd80ea13e964a5a9f728a68567dc784".into(),
            "https://dai.poa.network".into(),
        )
    }

    fn eth_to_wei(eth: f64) -> Uint256 {
        let wei = (eth * 1000000000000000000f64) as u64;
        wei.into()
    }

    #[test]
    fn get_block() {
        let system = actix::System::new("test");
        let tb = new_token_bridge();

        actix::spawn(
            tb.eth_web3
                .eth_block_number()
                .and_then({
                    let web3 = tb.eth_web3.clone();
                    move |current_block_number| web3.eth_get_block_by_number(current_block_number)
                })
                .then(|derp| {
                    println!("{:?}", derp);
                    actix::System::current().stop();
                    Box::new(futures::future::ok(()))
                }),
        );

        system.run();
    }

    #[test]
    fn eth_to_xdai() {
        let system = actix::System::new("test");

        actix::spawn(
            new_token_bridge()
                .convert_eth_to_xdai(eth_to_wei(0.001f64))
                .then(|derp| {
                    println!("{:?}", derp);
                    actix::System::current().stop();
                    Box::new(futures::future::ok(()))
                }),
        );

        system.run();
    }
}
