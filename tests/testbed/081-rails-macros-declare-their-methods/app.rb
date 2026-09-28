class Account
  has_secure_password
  has_secure_token :api_key
  has_one_attached :avatar
  has_many_attached :documents
  has_one :profile
  belongs_to :team
  accepts_nested_attributes_for :profile
  store :settings, accessors: [:theme], coder: JSON
  store_accessor :settings, :locale, prefix: true
  attribute :nickname
  alias_attribute :handle, :nickname
end

class Job
  def run(id)
    account = Account.find(id)
    account.authenticate("pw")
    account.regenerate_api_key
    account.avatar.attach(nil)
    account.documents_blobs
    account.profile_attributes = {}
    account.theme
    account.settings_locale_changed?
    account.handle
    account.name_changed?
    account.team_previously_changed?
    account.reset_profile
    Account.with_attached_avatar
    account.settings
    account.nickname_changed?
  end
end
