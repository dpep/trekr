class Part < ActiveRecord::Base
  def self.popular
    order(:rank)
  end

  def self.recent
    order(:id)
  end
end
