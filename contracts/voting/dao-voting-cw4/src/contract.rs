#[cfg(not(feature = "library"))]
use cosmwasm_std::entry_point;
use cosmwasm_std::{
    to_json_binary, Binary, Deps, DepsMut, Env, MessageInfo, Reply, Response, StdResult, SubMsg,
    Uint128, Uint256, WasmMsg,
};
use cw2::{get_contract_version, set_contract_version, ContractVersion};
use cw4::{MemberListResponse, MemberResponse, TotalWeightResponse};
use cw_utils::parse_reply_instantiate_data;

use dao_interface::voting::IsActiveResponse;
use dao_voting::threshold::ActiveThreshold;

use crate::error::ContractError;
use crate::msg::{ExecuteMsg, GroupContract, InstantiateMsg, MigrateMsg, QueryMsg};
use crate::state::{Config, ACTIVE_THRESHOLD, CONFIG, DAO, GROUP_CONTRACT};

pub(crate) const CONTRACT_NAME: &str = "crates.io:dao-voting-cw4";
pub(crate) const CONTRACT_VERSION: &str = env!("CARGO_PKG_VERSION");

const INSTANTIATE_GROUP_REPLY_ID: u64 = 0;

#[cfg_attr(not(feature = "library"), entry_point)]
pub fn instantiate(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    msg: InstantiateMsg,
) -> Result<Response, ContractError> {
    set_contract_version(deps.storage, CONTRACT_NAME, CONTRACT_VERSION)?;

    let config: Config = if let Some(active_threshold) = msg.active_threshold {
        Config {
            active_threshold: Some(active_threshold),
        }
    } else {
        Config {
            active_threshold: None,
        }
    };

    CONFIG.save(deps.storage, &config)?;

    DAO.save(deps.storage, &info.sender)?;

    match msg.group_contract {
        GroupContract::New {
            cw4_group_code_id,
            initial_members,
        } => {
            if initial_members.is_empty() {
                return Err(ContractError::NoMembers {});
            }
            let original_len = initial_members.len();
            let mut initial_members = initial_members;
            initial_members.sort_by(|a, b| a.addr.cmp(&b.addr));
            initial_members.dedup();
            let new_len = initial_members.len();

            if original_len != new_len {
                return Err(ContractError::DuplicateMembers {});
            }

            let mut total_weight = Uint128::zero();
            for member in initial_members.iter() {
                deps.api.addr_validate(&member.addr)?;
                if member.weight > 0 {
                    // This works because query_voting_power_at_height will return 0 on address missing
                    // from storage, so no need to store anything.
                    let weight = Uint128::from(member.weight);
                    total_weight += weight;
                }
            }

            if total_weight.is_zero() {
                return Err(ContractError::ZeroTotalWeight {});
            }

            // Instantiate group contract, set DAO as admin.
            // Voting module contracts are instantiated by the main dao-dao-core
            // contract, so the Admin is set to info.sender.
            let msg = WasmMsg::Instantiate {
                admin: Some(info.sender.to_string()),
                code_id: cw4_group_code_id,
                msg: to_json_binary(&cw4_group::msg::InstantiateMsg {
                    admin: Some(info.sender.to_string()),
                    members: initial_members,
                })?,
                funds: vec![],
                label: env.contract.address.to_string(),
            };

            let msg = SubMsg::reply_on_success(msg, INSTANTIATE_GROUP_REPLY_ID);

            Ok(Response::new()
                .add_attribute("action", "instantiate")
                .add_submessage(msg))
        }
        GroupContract::Existing { address } => {
            let group_contract = deps.api.addr_validate(&address)?;

            // Validate valid group contract that has at least one member.
            let res: MemberListResponse = deps.querier.query_wasm_smart(
                group_contract.clone(),
                &cw4_group::msg::QueryMsg::ListMembers {
                    start_after: None,
                    limit: Some(1),
                },
            )?;

            if res.members.is_empty() {
                return Err(ContractError::NoMembers {});
            }

            GROUP_CONTRACT.save(deps.storage, &group_contract)?;

            Ok(Response::new()
                .add_attribute("action", "instantiate")
                .add_attribute("group_contract", group_contract.to_string()))
        }
    }
}

#[cfg_attr(not(feature = "library"), entry_point)]
pub fn execute(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    msg: ExecuteMsg,
) -> Result<Response, ContractError> {
    match msg {
        ExecuteMsg::UpdateActiveThreshold { new_threshold } => {
            execute_update_active_threshold(deps, env, info, new_threshold)
        }
    }
}

pub fn execute_update_active_threshold(
    deps: DepsMut,
    _env: Env,
    info: MessageInfo,
    new_active_threshold: Option<ActiveThreshold>,
) -> Result<Response, ContractError> {
    let dao = DAO.load(deps.storage)?;
    if info.sender != dao {
        return Err(ContractError::Unauthorized {});
    }

    if let Some(active_threshold) = new_active_threshold {
        match active_threshold {
            ActiveThreshold::AbsoluteCount { count } => {
                if count.is_zero() {
                    return Err(ContractError::InvalidThreshold {});
                }
                ACTIVE_THRESHOLD.save(deps.storage, &active_threshold)?;
            }
            // Reject percentage-based thresholds
            ActiveThreshold::Percentage { .. } => {
                return Err(ContractError::InvalidThreshold {});
            }
        }
    } else {
        ACTIVE_THRESHOLD.remove(deps.storage);
    }

    Ok(Response::new()
        .add_attribute("method", "update_active_threshold")
        .add_attribute("status", "success"))
}

#[cfg_attr(not(feature = "library"), entry_point)]
pub fn query(deps: Deps, env: Env, msg: QueryMsg) -> StdResult<Binary> {
    match msg {
        QueryMsg::VotingPowerAtHeight { address, height } => {
            query_voting_power_at_height(deps, env, address, height)
        }
        QueryMsg::TotalPowerAtHeight { height } => query_total_power_at_height(deps, env, height),
        QueryMsg::Info {} => query_info(deps),
        QueryMsg::GroupContract {} => to_json_binary(&GROUP_CONTRACT.load(deps.storage)?),
        QueryMsg::Dao {} => to_json_binary(&DAO.load(deps.storage)?),
        QueryMsg::IsActive {} => query_is_active(deps),
    }
}

pub fn query_voting_power_at_height(
    deps: Deps,
    env: Env,
    address: String,
    height: Option<u64>,
) -> StdResult<Binary> {
    let addr = deps.api.addr_validate(&address)?.to_string();
    let group_contract = GROUP_CONTRACT.load(deps.storage)?;
    let res: MemberResponse = deps.querier.query_wasm_smart(
        group_contract,
        &cw4_group::msg::QueryMsg::Member {
            addr,
            at_height: height,
        },
    )?;

    to_json_binary(&dao_interface::voting::VotingPowerAtHeightResponse {
        power: res.weight.unwrap_or(0).into(),
        height: height.unwrap_or(env.block.height),
    })
}

pub fn query_total_power_at_height(deps: Deps, env: Env, height: Option<u64>) -> StdResult<Binary> {
    let group_contract = GROUP_CONTRACT.load(deps.storage)?;
    let res: TotalWeightResponse = deps.querier.query_wasm_smart(
        group_contract,
        &cw4_group::msg::QueryMsg::TotalWeight { at_height: height },
    )?;
    to_json_binary(&dao_interface::voting::TotalPowerAtHeightResponse {
        power: res.weight.into(),
        height: height.unwrap_or(env.block.height),
    })
}

pub fn query_info(deps: Deps) -> StdResult<Binary> {
    let info = get_contract_version(deps.storage)?;
    to_json_binary(&dao_interface::voting::InfoResponse { info })
}

pub fn query_is_active(deps: Deps) -> StdResult<Binary> {
    let active_threshold = ACTIVE_THRESHOLD.may_load(deps.storage)?;

    match active_threshold {
        Some(ActiveThreshold::AbsoluteCount { count }) => {
            let group_contract = GROUP_CONTRACT.load(deps.storage)?;
            let total_weight: TotalWeightResponse = deps.querier.query_wasm_smart(
                &group_contract,
                &cw4_group::msg::QueryMsg::TotalWeight { at_height: None },
            )?;
            to_json_binary(&IsActiveResponse {
                active: total_weight.weight >= count.u128() as u64,
            })
        }
        Some(ActiveThreshold::Percentage { percent }) => {
            let group_contract = GROUP_CONTRACT.load(deps.storage)?;
            let total_weight: TotalWeightResponse = deps.querier.query_wasm_smart(
                &group_contract,
                &cw4_group::msg::QueryMsg::TotalWeight { at_height: None },
            )?;
            let percentage_base = Uint256::from(10u64).pow(percent.decimal_places()); // Ensure correct power base for scaling
            let required_weight = Uint256::from(total_weight.weight)
                .multiply_ratio(percent.atomics(), percentage_base); // Correct ratio calculation

            to_json_binary(&IsActiveResponse {
                active: Uint256::from(total_weight.weight) >= required_weight,
            })
        }
        None => to_json_binary(&IsActiveResponse { active: true }),
    }
}

#[cfg_attr(not(feature = "library"), entry_point)]
pub fn migrate(deps: DepsMut, _env: Env, _msg: MigrateMsg) -> Result<Response, ContractError> {
    let config: Config = CONFIG.load(deps.storage)?;
    // Update config as necessary
    CONFIG.save(deps.storage, &config)?;

    let storage_version: ContractVersion = get_contract_version(deps.storage)?;

    // Only migrate if newer
    if storage_version.version.as_str() < CONTRACT_VERSION {
        // Set contract to version to latest
        set_contract_version(deps.storage, CONTRACT_NAME, CONTRACT_VERSION)?;
    }

    Ok(Response::new().add_attribute("action", "migrate"))
}

#[cfg_attr(not(feature = "library"), entry_point)]
pub fn reply(deps: DepsMut, _env: Env, msg: Reply) -> Result<Response, ContractError> {
    match msg.id {
        INSTANTIATE_GROUP_REPLY_ID => {
            let res = parse_reply_instantiate_data(msg);
            match res {
                Ok(res) => {
                    let group_contract = GROUP_CONTRACT.may_load(deps.storage)?;
                    if group_contract.is_some() {
                        return Err(ContractError::DuplicateGroupContract {});
                    }
                    let group_contract = deps.api.addr_validate(&res.contract_address)?;
                    GROUP_CONTRACT.save(deps.storage, &group_contract)?;
                    Ok(Response::default().add_attribute("group_contract", group_contract))
                }
                Err(_) => Err(ContractError::GroupContractInstantiateError {}),
            }
        }
        _ => Err(ContractError::UnknownReplyId { id: msg.id }),
    }
}
