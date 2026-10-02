module Jobs
  class Retrier
    def again
      Jobs.enqueue(:retrier)
    end
  end
end
