class Widget < ApplicationRecord
  KINDS = { small: 1, large: 2 }.freeze
  STATES = %i[draft live].freeze
  FIELDS = %i[size].freeze
  UNUSED = 1

  enum kind: KINDS
  enum :state, STATES
  attr_reader(*FIELDS)
end
