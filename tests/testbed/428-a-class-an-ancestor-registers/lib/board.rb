class Board
  def scores
    Scorer.subclasses.map(&:new)
  end
end
