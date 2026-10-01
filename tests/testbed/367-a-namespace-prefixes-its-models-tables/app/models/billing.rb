module Billing
  def self.table_name_prefix
    "billing_"
  end

  def self.use_relative_model_naming?
    true
  end

  def self.lonely
    :lonely
  end
end
