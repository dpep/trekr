class Job
  def run
    ["a", "bb"].max_by(2) { |w| w.size }.flatten
    ["a", "bb"].max_by { |w| w.size }.size
    "abc".gsub!(/b/).with_index
    "abc".gsub!(/b/, "c").size
  end
end
