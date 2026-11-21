class Widget
  KINDS = [ALPHA = 1, BETA = 2].freeze
  SIZES = { small: SMALL = 1 }.freeze
  LONE = 3

  def kinds
    KINDS + SIZES.values
  end
end
