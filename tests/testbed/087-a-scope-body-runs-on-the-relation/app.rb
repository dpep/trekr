class Widget < ActiveRecord::Base
  scope :cheap, -> { where(price: 1) }
  scope :cheap_and_blue, -> { cheap.where(color: "blue") }
  scope :featured, lambda { where(id: featured_ids) }

  def self.featured_ids
    []
  end

  def self.plain
    where(price: 2)
  end
end
