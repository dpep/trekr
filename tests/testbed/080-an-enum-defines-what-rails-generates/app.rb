class Account
  enum :role, { member: 0, admin: 1 }
  enum :plan, [:free, :paid], prefix: true
  enum :tier, { bronze: 0, silver: 1 }, suffix: :level, scopes: false
  enum status: { active: 0, "on hold": 1 }, _prefix: :state
  enum :kind, { basic: 0 }, instance_methods: false

  def check
    role.upcase
    admin?
    plan_paid!
    bronze_level?
    state_on_hold?
    basic?
  end

  def self.listing
    admin
    not_admin
    not_plan_paid
    silver_level
    roles
  end
end
