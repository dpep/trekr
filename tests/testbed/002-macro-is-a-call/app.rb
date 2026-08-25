class Widget < ApplicationRecord
  belongs_to :supplier
  attr_reader :label
  delegate :region, to: :supplier, prefix: true
end
